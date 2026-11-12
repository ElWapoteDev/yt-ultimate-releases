//! Fixed BS-RoFormer inference. Neural network operations require Metal.

mod dsp;
mod model;

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use dsp::{SpectralTransform, BINS, FFT_HOP};
use model::{BandSplit, MaskEstimator, RmsNorm, Transformer};
use rustfft::num_complex::Complex32;
use std::{fs, path::Path};

pub const WINDOW_FRAMES: usize = 352_800;
const CHANNELS: usize = 2;
const BAND_WIDTHS: [usize; 62] = [
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 4, 4, 4, 4, 4, 4, 4, 4,
    4, 4, 4, 4, 12, 12, 12, 12, 12, 12, 12, 12, 24, 24, 24, 24, 24, 24, 24, 24, 48, 48, 48, 48, 48,
    48, 48, 48, 128, 129,
];
const BANDS: usize = BAND_WIDTHS.len();
const DIM: usize = 512;
const DEPTH: usize = 12;
const FRAMES: usize = WINDOW_FRAMES / FFT_HOP + 1;
const FULL_BINS: usize = BINS * CHANNELS;

pub struct BsRoFormer {
    device: Device,
    spectral: SpectralTransform,
    band_split: BandSplit,
    layers: Vec<(Transformer, Transformer)>,
    mask_estimator: MaskEstimator,
    final_norm: RmsNorm,
}

impl BsRoFormer {
    pub fn load(weights: &Path) -> candle_core::Result<Self> {
        // Candle indexes the device list directly, so check before initializing.
        if candle_metal_kernels::metal::Device::all().is_empty() {
            candle_core::bail!("La separación requiere una GPU compatible con Apple Metal.");
        }
        let device = Device::new_metal(0)?;
        let vb = VarBuilder::from_buffered_safetensors(fs::read(weights)?, DType::F32, &device)?;
        let input_dims: Vec<_> = BAND_WIDTHS
            .iter()
            .map(|width| width * CHANNELS * 2)
            .collect();
        let band_split = BandSplit::new(&input_dims, vb.pp("band_split"))?;
        let mut layers = Vec::with_capacity(DEPTH);
        for i in 0..DEPTH {
            let vb = vb.pp("layers").pp(i.to_string());
            layers.push((
                Transformer::new(FRAMES, vb.pp("0"))?,
                Transformer::new(BANDS, vb.pp("1"))?,
            ));
        }
        let mask_estimator = MaskEstimator::new(&input_dims, vb.pp("mask_estimators.0"))?;
        Ok(Self {
            device,
            spectral: SpectralTransform::new(),
            band_split,
            layers,
            mask_estimator,
            final_norm: RmsNorm::new(DIM, vb.pp("final_norm"))?,
        })
    }

    /// Predict the vocal waveform from an eight-second planar stereo window.
    pub fn predict(&self, input: &[f32], output: &mut [f32]) -> candle_core::Result<()> {
        if input.len() != WINDOW_FRAMES * CHANNELS || output.len() != input.len() {
            candle_core::bail!("Se requiere una ventana estéreo de ocho segundos.");
        }
        let spectrum = self.spectrum(input)?;
        let bands = spectrum.transpose(0, 1)?.reshape((FRAMES, FULL_BINS * 2))?;
        let mut features = self.band_split.forward(&bands)?;
        for (time, frequency) in &self.layers {
            // The other axis is a batch dimension. Chunking bounds memory
            // without shortening the attention context on the active axis.
            let transposed = features.transpose(0, 1)?.contiguous()?;
            let temporal = time.forward_batched(&transposed, 8)?;
            features = frequency.forward_batched(&temporal.transpose(0, 1)?.contiguous()?, 64)?;
        }

        let masks = self
            .mask_estimator
            .forward(&self.final_norm.forward(&features)?)?;
        let masks = masks.reshape((FRAMES, FULL_BINS, 2))?.transpose(0, 1)?;
        let real_mask = masks.narrow(2, 0, 1)?.squeeze(2)?;
        let imag_mask = masks.narrow(2, 1, 1)?.squeeze(2)?;
        let real = spectrum.narrow(2, 0, 1)?.squeeze(2)?;
        let imag = spectrum.narrow(2, 1, 1)?.squeeze(2)?;
        let real_out = ((&real * &real_mask)? - (&imag * &imag_mask)?)?;
        let imag_out = ((&real * &imag_mask)? + (&imag * &real_mask)?)?;
        let real_out = real_out.flatten_all()?.to_vec1::<f32>()?;
        let imag_out = imag_out.flatten_all()?.to_vec1::<f32>()?;
        let mut spectral_channel = vec![Complex32::default(); FRAMES * BINS];
        for (channel, waveform) in output.chunks_exact_mut(WINDOW_FRAMES).enumerate() {
            for (frame, bins) in spectral_channel.chunks_exact_mut(BINS).enumerate() {
                for (bin, value) in bins.iter_mut().enumerate() {
                    let index = (bin * CHANNELS + channel) * FRAMES + frame;
                    *value = Complex32::new(real_out[index], imag_out[index]);
                }
            }
            self.spectral.inverse(&spectral_channel, waveform);
        }
        Ok(())
    }

    fn spectrum(&self, input: &[f32]) -> candle_core::Result<Tensor> {
        let mut data = vec![0.0_f32; FULL_BINS * FRAMES * 2];
        for (channel, waveform) in input.chunks_exact(WINDOW_FRAMES).enumerate() {
            for (frame, bins) in self
                .spectral
                .forward(waveform)
                .chunks_exact(BINS)
                .enumerate()
            {
                for (bin, value) in bins.iter().enumerate() {
                    let index = ((bin * CHANNELS + channel) * FRAMES + frame) * 2;
                    data[index] = value.re;
                    data[index + 1] = value.im;
                }
            }
        }
        Tensor::from_vec(data, (FULL_BINS, FRAMES, 2), &self.device)
    }
}
