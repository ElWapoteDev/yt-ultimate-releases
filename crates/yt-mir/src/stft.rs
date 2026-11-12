//! The short-time Fourier transform exactly as HTDemucs computes it around its
//! network (`demucs.htdemucs.HTDemucs._spec` and `_ispec`, over `torch.stft`
//! and `torch.istft`): a 4,096-point periodic Hann window, a hop of 1,024,
//! unitary scaling, reflect padding that centres frame `t` on sample
//! `t × HOP`, and the Nyquist bin dropped.

use realfft::{num_complex::Complex, ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Window and transform length, in samples.
pub(crate) const N_FFT: usize = 4_096;
/// Samples between frames.
pub(crate) const HOP: usize = N_FFT / 4;
/// Frequency bins kept (the Nyquist bin is dropped).
pub(crate) const BINS: usize = N_FFT / 2;
/// Reflect padding HTDemucs adds on each side before the transform's own,
/// so its frames line up with the network's time branch.
const PAD: usize = HOP / 2 * 3;
/// Frames the transform adds on each side of the ones HTDemucs keeps.
const EDGE_FRAMES: usize = 2;
/// Window sums below this leave the sample silent (as `torch.istft` checks).
const MIN_ENVELOPE: f32 = 1e-11;

pub(crate) struct Stft {
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    window: Vec<f32>,
    frame: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch_forward: Vec<Complex<f32>>,
    scratch_inverse: Vec<Complex<f32>>,
}

impl Stft {
    pub(crate) fn new() -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(N_FFT);
        let inverse = planner.plan_fft_inverse(N_FFT);
        let window = (0..N_FFT)
            .map(|n| 0.5 - 0.5 * (std::f32::consts::TAU * n as f32 / N_FFT as f32).cos())
            .collect();
        Self {
            frame: forward.make_input_vec(),
            spectrum: forward.make_output_vec(),
            scratch_forward: forward.make_scratch_vec(),
            scratch_inverse: inverse.make_scratch_vec(),
            forward,
            inverse,
            window,
        }
    }

    /// Frames of the spectrogram of `length` samples.
    pub(crate) fn frames(length: usize) -> usize {
        length.div_ceil(HOP)
    }

    /// The spectrogram of one channel into `re` and `im`, each
    /// `BINS × frames(channel.len())` with frames contiguous per bin.
    /// `channel` must be longer than [`N_FFT`].
    pub(crate) fn forward(&mut self, channel: &[f32], re: &mut [f32], im: &mut [f32]) {
        let frames = Self::frames(channel.len());
        let padded = frames * HOP + 2 * PAD;
        let sample = |index: usize| {
            let inner = reflect(index as isize - (N_FFT / 2) as isize, padded);
            channel[reflect(inner as isize - PAD as isize, channel.len())]
        };
        let scale = 1.0 / (N_FFT as f32).sqrt();
        for frame in 0..frames {
            let start = (frame + EDGE_FRAMES) * HOP;
            for (n, value) in self.frame.iter_mut().enumerate() {
                *value = sample(start + n) * self.window[n];
            }
            // The buffers come from the plan, so their lengths always match it.
            let _ = self.forward.process_with_scratch(
                &mut self.frame,
                &mut self.spectrum,
                &mut self.scratch_forward,
            );
            for (bin, value) in self.spectrum.iter().take(BINS).enumerate() {
                re[bin * frames + frame] = value.re * scale;
                im[bin * frames + frame] = value.im * scale;
            }
        }
    }

    /// The `length` samples whose spectrogram is `re`, `im` (as
    /// [`Self::forward`] lays it out for `length`).
    pub(crate) fn inverse(&mut self, re: &[f32], im: &[f32], length: usize) -> Vec<f32> {
        let frames = Self::frames(length);
        let total = frames + 2 * EDGE_FRAMES;
        let mut sum = vec![0.0_f32; N_FFT + HOP * (total - 1)];
        let mut envelope = vec![0.0_f32; sum.len()];
        let scale = 1.0 / (N_FFT as f32).sqrt();
        for frame in 0..total {
            let start = frame * HOP;
            for (n, weight) in self.window.iter().enumerate() {
                envelope[start + n] += weight * weight;
            }
            let Some(kept) = frame.checked_sub(EDGE_FRAMES).filter(|kept| *kept < frames) else {
                continue;
            };
            for (bin, value) in self.spectrum.iter_mut().enumerate() {
                *value = if bin < BINS {
                    Complex::new(re[bin * frames + kept], im[bin * frames + kept])
                } else {
                    Complex::new(0.0, 0.0)
                };
            }
            // A real signal has no imaginary part at DC; `torch.istft` ignores it too.
            self.spectrum[0].im = 0.0;
            let _ = self.inverse.process_with_scratch(
                &mut self.spectrum,
                &mut self.frame,
                &mut self.scratch_inverse,
            );
            for (n, value) in self.frame.iter().enumerate() {
                sum[start + n] += value * scale * self.window[n];
            }
        }
        let offset = N_FFT / 2 + PAD;
        (0..length)
            .map(|n| {
                let weight = envelope[offset + n];
                if weight > MIN_ENVELOPE {
                    sum[offset + n] / weight
                } else {
                    0.0
                }
            })
            .collect()
    }
}

/// `index` reflected into `0..length` without repeating the edge sample, as
/// `torch`'s reflect padding does (for padding shorter than `length`).
fn reflect(index: isize, length: usize) -> usize {
    let last = length as isize - 1;
    let reflected = if index < 0 {
        -index
    } else if index > last {
        2 * last - index
    } else {
        index
    };
    reflected.clamp(0, last) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(length: usize) -> Vec<f32> {
        (0..length)
            .map(|n| {
                let t = n as f32 / 44_100.0;
                0.5 * (std::f32::consts::TAU * 440.0 * t).sin()
                    + 0.25 * (std::f32::consts::TAU * 3_000.0 * t + 1.0).sin()
            })
            .collect()
    }

    #[test]
    fn the_inverse_returns_the_signal_away_from_its_edges() {
        // HTDemucs drops two frames at each edge, so (as in `torch`) the first
        // and last few thousand samples come back attenuated; the blending of
        // overlapping segments gives them almost no weight.
        let length = 30_000;
        let input = signal(length);
        let mut stft = Stft::new();
        let frames = Stft::frames(length);
        let (mut re, mut im) = (vec![0.0; BINS * frames], vec![0.0; BINS * frames]);
        stft.forward(&input, &mut re, &mut im);
        let output = stft.inverse(&re, &im, length);
        let error = input[N_FFT..length - N_FFT]
            .iter()
            .zip(&output[N_FFT..length - N_FFT])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(error < 1e-4, "{error}");
    }

    #[test]
    fn a_sine_lands_in_its_bin_with_unitary_scaling() {
        let length = 20 * HOP;
        let bin = 40;
        let input: Vec<f32> = (0..length)
            .map(|n| (std::f32::consts::TAU * bin as f32 * n as f32 / N_FFT as f32).cos())
            .collect();
        let mut stft = Stft::new();
        let frames = Stft::frames(length);
        let (mut re, mut im) = (vec![0.0; BINS * frames], vec![0.0; BINS * frames]);
        stft.forward(&input, &mut re, &mut im);
        let magnitude =
            |bin: usize, frame: usize| re[bin * frames + frame].hypot(im[bin * frames + frame]);
        let middle = frames / 2;
        // A unit cosine through a Hann window: N/4 before the 1/√N scaling.
        let expected = (N_FFT as f32).sqrt() / 4.0;
        assert!((magnitude(bin, middle) - expected).abs() < 0.01 * expected);
        assert!(magnitude(bin + 3, middle) < 1e-3 * expected);
    }

    #[test]
    fn reflection_skips_the_edge_sample() {
        assert_eq!(reflect(-1, 10), 1);
        assert_eq!(reflect(-3, 10), 3);
        assert_eq!(reflect(10, 10), 8);
        assert_eq!(reflect(4, 10), 4);
    }
}
