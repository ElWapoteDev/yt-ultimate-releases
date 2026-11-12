//! Validated input samples, the mono mix and resampling to the beat model's rate.

use crate::{MirError, Result};
use rubato::{FftFixedInOut, Resampler};
use std::ops::RangeInclusive;
use yt_core::TrackAnalysis;

/// Sample rate the beat model was trained on, in Hz.
pub(crate) const MODEL_RATE: u32 = 22_050;
/// Input frames per resampler call.
const RESAMPLE_CHUNK: usize = 4_096;

/// Channel-interleaved `f32` samples, validated when created.
#[derive(Debug, Clone, Copy)]
pub struct Pcm<'a> {
    samples: &'a [f32],
    channels: u16,
    sample_rate: u32,
}

impl<'a> Pcm<'a> {
    /// Channel counts accepted.
    pub const CHANNELS: RangeInclusive<u16> = 1..=8;
    /// Shortest audio analyzed, in seconds: the beat model needs a few bars.
    pub const MIN_SECONDS: f64 = 5.0;

    /// Wrap `samples`; fails unless the channel count and rate are in range,
    /// the samples are whole frames of finite values, and the duration lies
    /// between [`Self::MIN_SECONDS`] and [`TrackAnalysis::MAX_DURATION_SECONDS`].
    pub fn new(samples: &'a [f32], channels: u16, sample_rate: u32) -> Result<Self> {
        let invalid = |what: &str| Err(MirError::InvalidAudio(what.into()));
        if !Self::CHANNELS.contains(&channels) {
            return invalid("the channel count is outside 1–8");
        }
        if !TrackAnalysis::SAMPLE_RATE.contains(&sample_rate) {
            return invalid("the sample rate is outside 8–192 kHz");
        }
        if !samples.len().is_multiple_of(usize::from(channels)) {
            return invalid("the samples are not whole frames");
        }
        let seconds = (samples.len() / usize::from(channels)) as f64 / f64::from(sample_rate);
        if seconds < Self::MIN_SECONDS {
            return invalid("the audio is too short to analyze");
        }
        if seconds > f64::from(TrackAnalysis::MAX_DURATION_SECONDS) {
            return invalid("the audio is longer than two hours");
        }
        if samples.iter().any(|sample| !sample.is_finite()) {
            return invalid("the audio contains non-finite samples");
        }
        Ok(Self {
            samples,
            channels,
            sample_rate,
        })
    }

    /// The interleaved samples.
    pub fn samples(&self) -> &'a [f32] {
        self.samples
    }

    /// Interleaved channels per frame.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Frames per second.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Frames (samples per channel).
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels)
    }

    /// The channel average, as Beat This! mixes its input.
    pub(crate) fn mono(&self) -> Vec<f32> {
        let channels = usize::from(self.channels);
        let scale = 1.0 / channels as f32;
        self.samples
            .chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() * scale)
            .collect()
    }
}

/// Resample mono `input` from `from` Hz to `to` Hz with an FFT resampler,
/// compensating its delay so sample `n` of the output is time `n / to`.
pub(crate) fn resample(input: &[f32], from: u32, to: u32) -> Result<Vec<f32>> {
    if from == to {
        return Ok(input.to_vec());
    }
    let failure = |error: &dyn std::fmt::Display| {
        MirError::InvalidAudio(format!("resampling failed: {error}"))
    };
    let mut resampler = FftFixedInOut::<f32>::new(from as usize, to as usize, RESAMPLE_CHUNK, 1)
        .map_err(|error| failure(&error))?;
    let delay = resampler.output_delay();
    let expected = (input.len() as u64 * u64::from(to) / u64::from(from)) as usize;
    let mut output = Vec::with_capacity(expected + delay + resampler.output_frames_max());
    let mut buffer = vec![vec![0.0_f32; resampler.output_frames_max()]];
    let mut position = 0;
    while position + resampler.input_frames_next() <= input.len() {
        let next = resampler.input_frames_next();
        let (_, written) = resampler
            .process_into_buffer(&[&input[position..position + next]], &mut buffer, None)
            .map_err(|error| failure(&error))?;
        output.extend_from_slice(&buffer[0][..written]);
        position += next;
    }
    // rubato refuses an empty partial chunk, which is what remains when the
    // input is a whole number of chunks; the flush below covers that case.
    if position < input.len() {
        let (_, written) = resampler
            .process_partial_into_buffer(Some(&[&input[position..]]), &mut buffer, None)
            .map_err(|error| failure(&error))?;
        output.extend_from_slice(&buffer[0][..written]);
    }
    while output.len() < delay + expected {
        let (_, written) = resampler
            .process_partial_into_buffer(None::<&[&[f32]]>, &mut buffer, None)
            .map_err(|error| failure(&error))?;
        if written == 0 {
            break;
        }
        output.extend_from_slice(&buffer[0][..written]);
    }
    output.drain(..delay.min(output.len()));
    output.resize(expected, 0.0);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn sine(rate: u32, hz: f32, seconds: f32) -> Vec<f32> {
        (0..(rate as f32 * seconds) as usize)
            .map(|n| (TAU * hz * n as f32 / rate as f32).sin())
            .collect()
    }

    #[test]
    fn pcm_rejects_what_cannot_be_analyzed() {
        let stereo = vec![0.0; 44_100 * 2 * 6];
        assert!(Pcm::new(&stereo, 2, 44_100).is_ok());
        assert!(Pcm::new(&stereo, 0, 44_100).is_err());
        assert!(Pcm::new(&stereo, 9, 44_100).is_err());
        assert!(Pcm::new(&stereo, 2, 4_000).is_err());
        assert!(Pcm::new(&stereo[..stereo.len() - 1], 2, 44_100).is_err());
        assert!(
            Pcm::new(&stereo[..44_100 * 2], 2, 44_100).is_err(),
            "too short"
        );
        let mut broken = stereo.clone();
        broken[10] = f32::NAN;
        assert!(Pcm::new(&broken, 2, 44_100).is_err());
    }

    #[test]
    fn mono_averages_the_channels() {
        let samples = [1.0, 0.0, 0.5, 0.5].repeat(44_100 * 3);
        let pcm = Pcm::new(&samples, 2, 44_100).unwrap();
        let mono = pcm.mono();
        assert_eq!(mono.len(), pcm.frames());
        assert!((mono[0] - 0.5).abs() < 1e-7 && (mono[1] - 0.5).abs() < 1e-7);
    }

    #[test]
    fn resampling_keeps_length_frequency_and_timing() {
        for from in [44_100, 48_000] {
            let input = sine(from, 1_000.0, 2.0);
            let output = resample(&input, from, MODEL_RATE).unwrap();
            assert_eq!(
                output.len(),
                (input.len() as u64 * 22_050 / u64::from(from)) as usize
            );
            // Fit a 1 kHz sinusoid to the middle: its amplitude must survive and
            // its phase (the remaining delay) stay under two samples (90 µs).
            let omega = TAU * 1_000.0 / MODEL_RATE as f32;
            let middle = output.len() / 2;
            let (mut sine_part, mut cosine_part) = (0.0_f32, 0.0_f32);
            for (n, value) in output.iter().enumerate().skip(middle - 2_000).take(4_000) {
                sine_part += value * (omega * n as f32).sin();
                cosine_part += value * (omega * n as f32).cos();
            }
            let amplitude = 2.0 * sine_part.hypot(cosine_part) / 4_000.0;
            let delay_samples = cosine_part.atan2(sine_part) / omega;
            assert!(
                (amplitude - 1.0).abs() < 0.01,
                "from {from}: amplitude {amplitude}"
            );
            assert!(
                delay_samples.abs() < 2.0,
                "from {from}: delay {delay_samples}"
            );
        }
        assert_eq!(
            resample(&[0.25; 10], 22_050, 22_050).unwrap(),
            vec![0.25; 10]
        );
    }

    #[test]
    fn input_of_a_whole_number_of_chunks_resamples() {
        let chunk = FftFixedInOut::<f32>::new(44_100, MODEL_RATE as usize, RESAMPLE_CHUNK, 1)
            .unwrap()
            .input_frames_next();
        for frames in [chunk * 3, chunk * 3 + 1, chunk * 3 - 1] {
            let input = vec![0.25_f32; frames];
            let output = resample(&input, 44_100, MODEL_RATE).unwrap();
            assert_eq!(
                output.len(),
                frames * MODEL_RATE as usize / 44_100,
                "{frames} frames"
            );
        }
    }
}
