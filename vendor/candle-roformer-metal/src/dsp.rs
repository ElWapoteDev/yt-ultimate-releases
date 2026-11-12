use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use std::{f32::consts::PI, sync::Arc};

pub(crate) const FFT_SIZE: usize = 2048;
pub(crate) const FFT_HOP: usize = 441;
pub(crate) const BINS: usize = FFT_SIZE / 2 + 1;
const PAD: usize = FFT_SIZE / 2;

pub(crate) struct SpectralTransform {
    window: Vec<f32>,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
}

impl SpectralTransform {
    pub(crate) fn new() -> Self {
        let mut planner = FftPlanner::new();
        Self {
            window: (0..FFT_SIZE)
                .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f32 / FFT_SIZE as f32).cos()))
                .collect(),
            fft: planner.plan_fft_forward(FFT_SIZE),
            ifft: planner.plan_fft_inverse(FFT_SIZE),
        }
    }

    pub(crate) fn forward(&self, input: &[f32]) -> Vec<Complex32> {
        let mut padded = Vec::with_capacity(input.len() + 2 * PAD);
        padded.extend(input[1..=PAD].iter().rev().copied());
        padded.extend_from_slice(input);
        padded.extend(
            input[input.len() - PAD - 1..input.len() - 1]
                .iter()
                .rev()
                .copied(),
        );
        let frames = padded.windows(FFT_SIZE).step_by(FFT_HOP);
        let mut spectrum = Vec::with_capacity(frames.len() * BINS);
        let mut buffer = vec![Complex32::default(); FFT_SIZE];
        let mut scratch = vec![Complex32::default(); self.fft.get_inplace_scratch_len()];
        for frame in frames {
            for ((sample, window), bin) in frame.iter().zip(&self.window).zip(&mut buffer) {
                *bin = Complex32::new(sample * window, 0.0);
            }
            self.fft.process_with_scratch(&mut buffer, &mut scratch);
            spectrum.extend_from_slice(&buffer[..BINS]);
        }
        spectrum
    }

    pub(crate) fn inverse(&self, spectrum: &[Complex32], output: &mut [f32]) {
        let frames = spectrum.chunks_exact(BINS);
        let padded_length = (frames.len() - 1) * FFT_HOP + FFT_SIZE;
        let mut padded = vec![0.0; padded_length];
        let mut weights = vec![0.0; padded_length];
        let mut buffer = vec![Complex32::default(); FFT_SIZE];
        let mut scratch = vec![Complex32::default(); self.ifft.get_inplace_scratch_len()];
        for (index, frame) in frames.enumerate() {
            buffer[..BINS].copy_from_slice(frame);
            for (bin, value) in buffer.iter_mut().enumerate().skip(BINS) {
                *value = frame[FFT_SIZE - bin].conj();
            }
            self.ifft.process_with_scratch(&mut buffer, &mut scratch);
            let start = index * FFT_HOP;
            for (i, (&sample, &window)) in buffer.iter().zip(&self.window).enumerate() {
                padded[start + i] += (sample.re * (1.0 / FFT_SIZE as f32)) * window;
                weights[start + i] += window * window;
            }
        }
        // Every sample inside the unpadded window has positive overlap weight.
        for (i, sample) in output.iter_mut().enumerate() {
            *sample = padded[PAD + i] / weights[PAD + i];
        }
    }
}
