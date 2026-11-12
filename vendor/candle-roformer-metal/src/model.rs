use crate::DIM;
use candle_core::{DType, Module, Result, Tensor, D};
use candle_nn::{linear, linear_no_bias, ops, Linear, VarBuilder};

const HEADS: usize = 8;
const HEAD_DIM: usize = 64;
const INNER_DIM: usize = HEADS * HEAD_DIM;
const EXPANDED_DIM: usize = 4 * DIM;

// Flatten leading dimensions to avoid a zero batch stride in the Metal GEMM.
fn project(layer: &Linear, input: &Tensor) -> Result<Tensor> {
    let features = input.dim(D::Minus1)?;
    let rows = input.elem_count() / features;
    let output = layer.forward(&input.contiguous()?.reshape((rows, features))?)?;
    let mut shape = input.dims().to_vec();
    let axis = shape.len() - 1;
    shape[axis] = output.dim(D::Minus1)?;
    output.reshape(shape)
}

pub(crate) struct RmsNorm {
    gamma: Tensor,
    epsilon: f32,
}

impl RmsNorm {
    pub(crate) fn new(features: usize, weights: VarBuilder) -> Result<Self> {
        Ok(Self {
            gamma: weights.get(features, "gamma")?,
            // The kernel uses the mean square. Scale the floor of the
            // reference L2 norm by the number of features.
            epsilon: 1e-24 / features as f32,
        })
    }

    pub(crate) fn forward(&self, input: &Tensor) -> Result<Tensor> {
        ops::rms_norm(&input.contiguous()?, &self.gamma, self.epsilon)
    }
}

struct RotaryEmbedding {
    cos: Tensor,
    sin: Tensor,
}

impl RotaryEmbedding {
    fn new(sequence: usize, weights: VarBuilder) -> Result<Self> {
        let frequencies = weights.get(HEAD_DIM / 2, "freqs")?;
        let positions =
            Tensor::arange(0i64, sequence as i64, weights.device())?.to_dtype(DType::F32)?;
        let angles = positions
            .unsqueeze(1)?
            .broadcast_mul(&frequencies.unsqueeze(0)?)?;
        Ok(Self {
            cos: angles.cos()?,
            sin: angles.sin()?,
        })
    }

    fn rotate(&self, input: &Tensor) -> Result<Tensor> {
        candle_nn::rotary_emb::rope_i(&input.contiguous()?, &self.cos, &self.sin)
    }
}

struct Attention {
    norm: RmsNorm,
    qkv: Linear,
    gates: Linear,
    out: Linear,
    rotary: RotaryEmbedding,
}

impl Attention {
    fn new(sequence: usize, weights: VarBuilder) -> Result<Self> {
        Ok(Self {
            norm: RmsNorm::new(DIM, weights.pp("norm"))?,
            qkv: linear_no_bias(DIM, 3 * INNER_DIM, weights.pp("to_qkv"))?,
            gates: linear(DIM, HEADS, weights.pp("to_gates"))?,
            out: linear_no_bias(INNER_DIM, DIM, weights.pp("to_out.0"))?,
            rotary: RotaryEmbedding::new(sequence, weights.pp("rotary_embed"))?,
        })
    }

    fn forward(&self, input: &Tensor) -> Result<Tensor> {
        let (batch, sequence, _) = input.dims3()?;
        let normalized = self.norm.forward(input)?;
        let qkv = project(&self.qkv, &normalized)?
            .reshape((batch, sequence, 3, HEADS, HEAD_DIM))?
            .permute((2, 0, 3, 1, 4))?;
        let q = self
            .rotary
            .rotate(&qkv.narrow(0, 0, 1)?.squeeze(0)?)?
            .contiguous()?;
        let k = self
            .rotary
            .rotate(&qkv.narrow(0, 1, 1)?.squeeze(0)?)?
            .contiguous()?;
        let v = qkv.narrow(0, 2, 1)?.squeeze(0)?.contiguous()?;
        let attended = ops::sdpa(
            &q,
            &k,
            &v,
            None,
            false,
            (HEAD_DIM as f32).sqrt().recip(),
            1.0,
        )?;
        let gates = project(&self.gates, &normalized)?
            .transpose(1, 2)?
            .unsqueeze(3)?;
        let attended = attended
            .broadcast_mul(&ops::sigmoid(&gates)?)?
            .transpose(1, 2)?
            .reshape((batch, sequence, INNER_DIM))?;
        project(&self.out, &attended)
    }
}

pub(crate) struct Transformer {
    attention: Attention,
    ff_norm: RmsNorm,
    ff_up: Linear,
    ff_down: Linear,
}

impl Transformer {
    pub(crate) fn new(sequence: usize, weights: VarBuilder) -> Result<Self> {
        let feed_forward = weights.pp("layers.0.1.net");
        Ok(Self {
            attention: Attention::new(sequence, weights.pp("layers.0.0"))?,
            ff_norm: RmsNorm::new(DIM, feed_forward.pp("0"))?,
            ff_up: linear(DIM, EXPANDED_DIM, feed_forward.pp("1"))?,
            ff_down: linear(EXPANDED_DIM, DIM, feed_forward.pp("4"))?,
        })
    }

    fn forward(&self, input: &Tensor) -> Result<Tensor> {
        let residual = (input + self.attention.forward(input)?)?;
        let normalized = self.ff_norm.forward(&residual)?;
        let hidden = project(&self.ff_up, &normalized)?.gelu_erf()?;
        residual + project(&self.ff_down, &hidden)?
    }

    pub(crate) fn forward_batched(&self, input: &Tensor, batch_size: usize) -> Result<Tensor> {
        let batches = input.dim(0)?;
        let mut outputs = Vec::with_capacity(batches.div_ceil(batch_size));
        for start in (0..batches).step_by(batch_size) {
            let batch = input.narrow(0, start, batch_size.min(batches - start))?;
            outputs.push(self.forward(&batch)?);
        }
        Tensor::cat(&outputs, 0)
    }
}

pub(crate) struct BandSplit(Vec<(usize, RmsNorm, Linear)>);

impl BandSplit {
    pub(crate) fn new(input_dims: &[usize], weights: VarBuilder) -> Result<Self> {
        let mut bands = Vec::with_capacity(input_dims.len());
        for (index, &size) in input_dims.iter().enumerate() {
            let weights = weights.pp("to_features").pp(index.to_string());
            bands.push((
                size,
                RmsNorm::new(size, weights.pp("0"))?,
                linear(size, DIM, weights.pp("1"))?,
            ));
        }
        Ok(Self(bands))
    }

    pub(crate) fn forward(&self, input: &Tensor) -> Result<Tensor> {
        let mut outputs = Vec::with_capacity(self.0.len());
        let mut offset = 0;
        for (width, norm, linear) in &self.0 {
            let band = input.narrow(1, offset, *width)?;
            outputs.push(project(linear, &norm.forward(&band)?)?);
            offset += width;
        }
        Tensor::stack(&outputs, 1)
    }
}

pub(crate) struct MaskEstimator(Vec<[Linear; 2]>);

impl MaskEstimator {
    pub(crate) fn new(input_dims: &[usize], weights: VarBuilder) -> Result<Self> {
        let mut bands = Vec::with_capacity(input_dims.len());
        for (index, &size) in input_dims.iter().enumerate() {
            let weights = weights.pp("to_freqs").pp(index.to_string()).pp("0");
            bands.push([
                linear(DIM, EXPANDED_DIM, weights.pp("0"))?,
                linear(EXPANDED_DIM, 2 * size, weights.pp("2"))?,
            ]);
        }
        Ok(Self(bands))
    }

    pub(crate) fn forward(&self, input: &Tensor) -> Result<Tensor> {
        let mut outputs = Vec::with_capacity(self.0.len());
        for (index, [first, last]) in self.0.iter().enumerate() {
            let band = input.narrow(1, index, 1)?.squeeze(1)?;
            let hidden = project(first, &band)?.tanh()?;
            let mask = project(last, &hidden)?;
            let half = mask.dim(1)? / 2;
            let a = mask.narrow(1, 0, half)?;
            let b = mask.narrow(1, half, half)?;
            outputs.push(a.broadcast_mul(&ops::sigmoid(&b)?)?);
        }
        Tensor::cat(&outputs, 1)
    }
}
