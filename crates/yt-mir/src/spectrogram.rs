//! The log-mel spectrogram Beat This! was trained on, and an onset envelope
//! derived from it.
//!
//! Matches `beat_this.preprocessing.LogMelSpect` (torchaudio `MelSpectrogram`):
//! a centred STFT with reflect padding and a periodic Hann window, magnitudes
//! scaled by `1/sqrt(N_FFT)` ("frame_length" normalization), a Slaney mel
//! filterbank without area normalization, then `ln(1 + 1000 · mel)`.

use crate::audio::MODEL_RATE;
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// FFT and window length, in samples at [`MODEL_RATE`].
pub(crate) const N_FFT: usize = 1_024;
/// Hop between frames, in samples: 20 ms, so the model sees 50 frames per second.
pub(crate) const HOP: usize = 441;
/// Mel bands per frame.
pub(crate) const N_MELS: usize = 128;
/// Frames per second of the spectrogram and of the model's predictions.
pub(crate) const FPS: f64 = MODEL_RATE as f64 / HOP as f64;
const F_MIN_HZ: f64 = 30.0;
const F_MAX_HZ: f64 = 11_000.0;
const LOG_MULTIPLIER: f32 = 1_000.0;
/// Slaney mel scale: linear below this frequency, logarithmic above, in Hz.
const SLANEY_BREAK_HZ: f64 = 1_000.0;
/// Hz per mel on the linear part of the Slaney scale.
const SLANEY_HZ_PER_MEL: f64 = 200.0 / 3.0;

/// One mel band: the first FFT bin it covers and its triangular weights.
struct MelBand {
    first_bin: usize,
    weights: Vec<f32>,
}

/// Reusable spectrogram plan: FFT, window and filterbank.
pub(crate) struct LogMel {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    bands: Vec<MelBand>,
}

impl LogMel {
    pub(crate) fn new() -> Self {
        let window = (0..N_FFT)
            .map(|n| (0.5 - 0.5 * (std::f64::consts::TAU * n as f64 / N_FFT as f64).cos()) as f32)
            .collect();
        Self {
            fft: RealFftPlanner::<f32>::new().plan_fft_forward(N_FFT),
            window,
            bands: mel_bands(),
        }
    }

    /// Frames produced for `samples` input samples (centred STFT).
    pub(crate) fn frames(samples: usize) -> usize {
        1 + samples / HOP
    }

    /// The spectrogram of `signal` (mono, [`MODEL_RATE`]) as `frames × N_MELS`
    /// row-major values. `signal` must be longer than half an FFT.
    pub(crate) fn compute(&self, signal: &[f32]) -> Vec<f32> {
        let frames = Self::frames(signal.len());
        let mut output = vec![0.0_f32; frames * N_MELS];
        let mut input = self.fft.make_input_vec();
        let mut spectrum = self.fft.make_output_vec();
        let mut magnitudes = vec![0.0_f32; spectrum.len()];
        let scale = 1.0 / (N_FFT as f32).sqrt();
        for frame in 0..frames {
            let start = (frame * HOP) as isize - (N_FFT / 2) as isize;
            for (n, slot) in input.iter_mut().enumerate() {
                *slot = signal[reflect(start + n as isize, signal.len())] * self.window[n];
            }
            // Lengths come from the plan itself, so processing cannot fail.
            if self.fft.process(&mut input, &mut spectrum).is_err() {
                continue;
            }
            for (magnitude, bin) in magnitudes.iter_mut().zip(&spectrum) {
                *magnitude = bin.norm() * scale;
            }
            let row = &mut output[frame * N_MELS..(frame + 1) * N_MELS];
            for (value, band) in row.iter_mut().zip(&self.bands) {
                let mel: f32 = band
                    .weights
                    .iter()
                    .zip(&magnitudes[band.first_bin..])
                    .map(|(weight, magnitude)| weight * magnitude)
                    .sum();
                *value = (LOG_MULTIPLIER * mel).ln_1p();
            }
        }
        output
    }
}

/// Mean positive change of the log-mel bands from the previous frame: high on
/// note and drum onsets, used to tell how beats subdivide.
pub(crate) fn onset_strength(spectrogram: &[f32], frames: usize) -> Vec<f32> {
    let mut strength = vec![0.0_f32; frames];
    for frame in 1..frames {
        let (previous, current) = (
            &spectrogram[(frame - 1) * N_MELS..frame * N_MELS],
            &spectrogram[frame * N_MELS..(frame + 1) * N_MELS],
        );
        strength[frame] = previous
            .iter()
            .zip(current)
            .map(|(before, now)| (now - before).max(0.0))
            .sum::<f32>()
            / N_MELS as f32;
    }
    strength
}

/// numpy "reflect" padding (the edge sample is not repeated) for `len > 1`.
fn reflect(index: isize, len: usize) -> usize {
    let period = 2 * (len as isize - 1);
    let wrapped = index.rem_euclid(period);
    (if wrapped >= len as isize {
        period - wrapped
    } else {
        wrapped
    }) as usize
}

fn hz_to_mel(hz: f64) -> f64 {
    if hz < SLANEY_BREAK_HZ {
        hz / SLANEY_HZ_PER_MEL
    } else {
        SLANEY_BREAK_HZ / SLANEY_HZ_PER_MEL + (hz / SLANEY_BREAK_HZ).ln() / (6.4_f64.ln() / 27.0)
    }
}

fn mel_to_hz(mel: f64) -> f64 {
    let break_mel = SLANEY_BREAK_HZ / SLANEY_HZ_PER_MEL;
    if mel < break_mel {
        mel * SLANEY_HZ_PER_MEL
    } else {
        SLANEY_BREAK_HZ * ((6.4_f64.ln() / 27.0) * (mel - break_mel)).exp()
    }
}

/// torchaudio `melscale_fbanks(513, 30, 11000, 128, 22050, norm=None, "slaney")`.
fn mel_bands() -> Vec<MelBand> {
    let bins = N_FFT / 2 + 1;
    // torchaudio spaces the bins up to the integer Nyquist frequency.
    let nyquist = f64::from(MODEL_RATE / 2);
    let frequencies: Vec<f64> = (0..bins)
        .map(|bin| nyquist * bin as f64 / (bins - 1) as f64)
        .collect();
    let (low, high) = (hz_to_mel(F_MIN_HZ), hz_to_mel(F_MAX_HZ));
    let points: Vec<f64> = (0..N_MELS + 2)
        .map(|index| mel_to_hz(low + (high - low) * index as f64 / (N_MELS + 1) as f64))
        .collect();
    (0..N_MELS)
        .map(|band| {
            let (left, center, right) = (points[band], points[band + 1], points[band + 2]);
            let weights: Vec<(usize, f32)> = frequencies
                .iter()
                .enumerate()
                .filter_map(|(bin, &hz)| {
                    let weight = ((hz - left) / (center - left))
                        .min((right - hz) / (right - center))
                        .max(0.0);
                    (weight > 0.0).then_some((bin, weight as f32))
                })
                .collect();
            let first_bin = weights.first().map_or(0, |(bin, _)| *bin);
            let last_bin = weights.last().map_or(0, |(bin, _)| *bin);
            let mut dense = vec![0.0_f32; last_bin + 1 - first_bin];
            for (bin, weight) in weights {
                dense[bin - first_bin] = weight;
            }
            MelBand {
                first_bin,
                weights: dense,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    #[test]
    fn reflect_matches_numpy() {
        let len = 5;
        let padded: Vec<usize> = (-3..8).map(|index| reflect(index, len)).collect();
        assert_eq!(padded, vec![3, 2, 1, 0, 1, 2, 3, 4, 3, 2, 1]);
    }

    #[test]
    fn the_slaney_scale_round_trips_and_bands_cover_30_hz_to_11_khz() {
        for hz in [30.0, 500.0, 1_000.0, 4_000.0, 11_000.0] {
            assert!((mel_to_hz(hz_to_mel(hz)) - hz).abs() < 1e-6 * hz);
        }
        let bands = mel_bands();
        assert_eq!(bands.len(), N_MELS);
        let bin_hz = f64::from(MODEL_RATE / 2) / (N_FFT / 2) as f64;
        assert!(bands[0].first_bin as f64 * bin_hz >= F_MIN_HZ - bin_hz);
        let last = bands.last().unwrap();
        assert!((last.first_bin + last.weights.len()) as f64 * bin_hz <= F_MAX_HZ + bin_hz);
        assert!(bands
            .iter()
            .all(|band| band.weights.iter().all(|w| (0.0..=1.0).contains(w))));
    }

    #[test]
    fn frame_count_and_tone_placement_follow_the_reference() {
        let samples = MODEL_RATE as usize * 3;
        let signal: Vec<f32> = (0..samples)
            .map(|n| 0.5 * (TAU * 1_000.0 * n as f32 / MODEL_RATE as f32).sin())
            .collect();
        let mel = LogMel::new();
        let spectrogram = mel.compute(&signal);
        assert_eq!(spectrogram.len(), LogMel::frames(samples) * N_MELS);
        assert_eq!(LogMel::frames(samples), 151);
        let row = &spectrogram[75 * N_MELS..76 * N_MELS];
        let loudest = row
            .iter()
            .enumerate()
            .fold((0, f32::MIN), |best, (index, &value)| {
                if value > best.1 {
                    (index, value)
                } else {
                    best
                }
            });
        let points: Vec<f64> = (0..N_MELS + 2)
            .map(|index| {
                mel_to_hz(
                    hz_to_mel(F_MIN_HZ)
                        + (hz_to_mel(F_MAX_HZ) - hz_to_mel(F_MIN_HZ)) * index as f64
                            / (N_MELS + 1) as f64,
                )
            })
            .collect();
        assert!(
            points[loudest.0] < 1_000.0 && 1_000.0 < points[loudest.0 + 2],
            "band {}",
            loudest.0
        );
        assert!(mel
            .compute(&vec![0.0; samples])
            .iter()
            .all(|value| *value == 0.0));
    }

    #[test]
    fn onsets_rise_where_sound_starts() {
        let frames = 4;
        let mut spectrogram = vec![0.0; frames * N_MELS];
        spectrogram[2 * N_MELS..3 * N_MELS].fill(1.0);
        let strength = onset_strength(&spectrogram, frames);
        assert_eq!(strength, vec![0.0, 0.0, 1.0, 0.0]);
    }
}
