//! How loud the vocals are, every half second, from the HTDemucs v4
//! separation network (Rouard, Massa and Défossez, 2023). The network is
//! exported without its STFT and ISTFT (`models/export_htdemucs.py`), which
//! [`crate::stft`] computes with identical math; the song is split into
//! segments of the length it was trained on, overlapping by a quarter and
//! blended with triangular weights as `demucs.apply.apply_model` does.
//! [`VocalModel::measure`] keeps only the vocal levels, dropping the audio as
//! it settles; [`VocalModel::separate`] returns the vocals of one window, for
//! a mix that plays them apart.

use crate::{
    audio::{resample, Pcm},
    error::runtime,
    model::{open, Device},
    stft::{Stft, BINS},
    MirError, Pace, Result, Stopwatch,
};
use ort::{session::Session, value::TensorRef};
use std::path::Path;
use yt_core::TrackAnalysis;

/// File name of the exported network (`models/export_htdemucs.py`).
pub const VOCAL_MODEL_FILENAME: &str = "htdemucs-core.onnx";
/// SHA-256 of the exported network; any other file is refused.
pub const VOCAL_MODEL_SHA256: &str =
    "6dc871812d60d6c3581e317c83231ac5c89a15aff7fd2506b3c7210aff5c6117";
/// Sample rate the network was trained on, in Hz.
const RATE: u32 = 44_100;
/// Samples per segment: the 39/5 s HTDemucs was trained on.
const SEGMENT: usize = 343_980;
/// Samples between segments: they overlap by a quarter.
const STRIDE: usize = SEGMENT * 3 / 4;
/// Channels the network takes and its sources (drums, bass, other, vocals).
const CHANNELS: usize = 2;
const SOURCES: usize = 4;
const VOCALS: usize = 3;
/// Level written for a silent step, in dBFS.
const SILENCE_DB: f32 = -120.0;
/// Seconds of the song around a separated window that the network also
/// hears, so the window's edges are separated like the rest.
const CONTEXT_SECONDS: f64 = 2.0;
const MIX_INPUT: &str = "mix";
const SPEC_INPUT: &str = "spec";
const SPEC_OUTPUT: &str = "spec_out";
const WAVE_OUTPUT: &str = "wave_out";

/// A loaded separation network, ready to measure vocals.
pub struct VocalModel {
    session: Session,
    device: Device,
    stft: Stft,
}

impl VocalModel {
    /// Check `path` against [`VOCAL_MODEL_SHA256`] and load it on `device`,
    /// keeping CoreML's compiled model in `cache` (macOS). Needs
    /// [`crate::init_runtime`] first.
    pub fn load(path: &Path, device: Device, cache: Option<&Path>) -> Result<Self> {
        let session = open(path, VOCAL_MODEL_SHA256, "vocal model", device, cache)?;
        Ok(Self {
            session,
            device,
            stft: Stft::new(),
        })
    }

    /// Where this model runs.
    pub fn device(&self) -> Device {
        self.device
    }

    /// RMS level of the vocals of `pcm` every
    /// [`TrackAnalysis::LOUDNESS_STEP_SECONDS`], in dBFS (both channels).
    /// Asks `pace` whether to go on after every segment and fails with
    /// [`MirError::Cancelled`] when it says no; [`MirError::Runtime`] or
    /// [`MirError::Model`] when the network cannot run or answers wrongly.
    pub fn measure(&mut self, pcm: &Pcm<'_>, pace: &dyn Pace) -> Result<Vec<f32>> {
        let mut watch = Stopwatch::new(pace);
        let channels = stereo(pcm.samples(), pcm.channels(), pcm.sample_rate())?;
        watch.lap()?;
        let step = (TrackAnalysis::LOUDNESS_STEP_SECONDS * RATE as f32) as usize;
        let mut meter = StepMeter::new(channels[0].len(), step);
        self.run(&channels, &mut meter, &mut watch)?;
        Ok(meter.levels())
    }

    /// The vocals of source frames `frames` of `pcm`, interleaved at its
    /// rate and channels, for a mix that plays them apart. Fails with
    /// [`MirError::InvalidAudio`] unless `pcm` is mono or stereo and covers
    /// `frames`; paced and failing like [`Self::measure`].
    pub fn separate(
        &mut self,
        pcm: &Pcm<'_>,
        frames: std::ops::Range<u64>,
        pace: &dyn Pace,
    ) -> Result<Vec<f32>> {
        let invalid = |what: &str| Err(MirError::InvalidAudio(what.into()));
        let source_channels = usize::from(pcm.channels());
        if source_channels > CHANNELS {
            return invalid("only mono or stereo audio is separated");
        }
        let total = pcm.frames() as u64;
        if frames.start >= frames.end || frames.end > total {
            return invalid("the window is outside the audio");
        }
        let rate = pcm.sample_rate();
        let context = (CONTEXT_SECONDS * f64::from(rate)) as u64;
        let first = frames.start.saturating_sub(context);
        let last = (frames.end + context).min(total);
        let samples =
            &pcm.samples()[first as usize * source_channels..last as usize * source_channels];
        let mut watch = Stopwatch::new(pace);
        let channels = stereo(samples, pcm.channels(), rate)?;
        watch.lap()?;
        let mut collected = Collected::default();
        self.run(&channels, &mut collected, &mut watch)?;
        let skip = (frames.start - first) as usize;
        let wanted = (frames.end - frames.start) as usize;
        let [left, right] = collected.0;
        let (left, right) = (resample(&left, RATE, rate)?, resample(&right, RATE, rate)?);
        let at = |channel: &[f32], frame: usize| channel.get(skip + frame).copied().unwrap_or(0.0);
        let mut out = Vec::with_capacity(wanted * source_channels);
        for frame in 0..wanted {
            let (l, r) = (at(&left, frame), at(&right, frame));
            if source_channels == 1 {
                out.push((l + r) / 2.0);
            } else {
                out.extend([l, r]);
            }
        }
        Ok(out)
    }

    /// Separate `channels` (stereo at [`RATE`]) segment by segment, handing
    /// every settled sample of the vocals to `settled`.
    fn run(
        &mut self,
        channels: &[Vec<f32>; 2],
        settled: &mut impl Settled,
        watch: &mut Stopwatch<'_>,
    ) -> Result<()> {
        let length = channels[0].len();
        let weight = triangle(SEGMENT);
        let frames = Stft::frames(SEGMENT);
        let mut mix = vec![0.0_f32; CHANNELS * SEGMENT];
        let mut spec = vec![0.0_f32; 2 * CHANNELS * BINS * frames];
        // Weighted vocals and weights from `base` on, until no later segment covers them.
        let mut blend = Blend::default();
        let mut offset = 0;
        while offset < length {
            let valid = (length - offset).min(SEGMENT);
            // As `TensorChunk.padded`: a short segment sits in the middle of
            // the network's input, between the song's neighbouring samples
            // (silence past its ends).
            let lead = (SEGMENT - valid) / 2;
            let start = offset as isize - lead as isize;
            for (channel, samples) in channels.iter().enumerate() {
                let row = &mut mix[channel * SEGMENT..(channel + 1) * SEGMENT];
                for (n, value) in row.iter_mut().enumerate() {
                    let index = start + n as isize;
                    *value = usize::try_from(index)
                        .ok()
                        .and_then(|index| samples.get(index))
                        .copied()
                        .unwrap_or(0.0);
                }
                let plane = BINS * frames;
                let (re, rest) = spec[2 * channel * plane..].split_at_mut(plane);
                self.stft.forward(row, re, &mut rest[..plane]);
            }
            let vocals = self.vocals(&mix, &spec, frames)?;
            blend.add(
                offset,
                valid,
                [&vocals[0][lead..], &vocals[1][lead..]],
                &weight,
            );
            // As `apply_model`, every stride starts a segment, even one the
            // previous segment already reaches the end from.
            blend.settle((offset + STRIDE).min(length), settled);
            watch.lap()?;
            offset += STRIDE;
        }
        Ok(())
    }

    /// Both channels of the vocals of one segment: the network's waveform
    /// branch plus the inverse transform of its spectrogram branch.
    fn vocals(&mut self, mix: &[f32], spec: &[f32], frames: usize) -> Result<[Vec<f32>; 2]> {
        let mix_input =
            TensorRef::from_array_view(([1_usize, CHANNELS, SEGMENT], mix)).map_err(runtime)?;
        let spec_input = TensorRef::from_array_view(([1_usize, 2 * CHANNELS, BINS, frames], spec))
            .map_err(runtime)?;
        let outputs = self
            .session
            .run(ort::inputs![MIX_INPUT => mix_input, SPEC_INPUT => spec_input])
            .map_err(runtime)?;
        let (_, spectrum) = outputs[SPEC_OUTPUT]
            .try_extract_tensor::<f32>()
            .map_err(runtime)?;
        let (_, wave) = outputs[WAVE_OUTPUT]
            .try_extract_tensor::<f32>()
            .map_err(runtime)?;
        let plane = BINS * frames;
        if spectrum.len() != SOURCES * 2 * CHANNELS * plane
            || wave.len() != SOURCES * CHANNELS * SEGMENT
        {
            return Err(MirError::Model(format!(
                "the vocal model answered {} and {} values",
                spectrum.len(),
                wave.len()
            )));
        }
        let mut out = [Vec::new(), Vec::new()];
        for (channel, samples) in out.iter_mut().enumerate() {
            let source = (VOCALS * 2 * CHANNELS + 2 * channel) * plane;
            let re = &spectrum[source..source + plane];
            let im = &spectrum[source + plane..source + 2 * plane];
            let mut audio = self.stft.inverse(re, im, SEGMENT);
            let start = (VOCALS * CHANNELS + channel) * SEGMENT;
            for (sample, time) in audio.iter_mut().zip(&wave[start..start + SEGMENT]) {
                *sample += time;
            }
            *samples = audio;
        }
        Ok(out)
    }
}

/// The first two channels of interleaved `samples` (a mono one twice) at
/// [`RATE`].
fn stereo(samples: &[f32], channels: u16, rate: u32) -> Result<[Vec<f32>; 2]> {
    let channels = usize::from(channels).max(1);
    let channel = |index: usize| -> Result<Vec<f32>> {
        let picked: Vec<f32> = samples
            .chunks_exact(channels)
            .map(|frame| frame[index.min(channels - 1)])
            .collect();
        resample(&picked, rate, RATE)
    };
    Ok([channel(0)?, channel(1)?])
}

/// Where the separated vocals go as they settle.
trait Settled {
    /// Sample `index` of the vocals, both channels.
    fn take(&mut self, index: usize, left: f32, right: f32);
}

impl Settled for StepMeter {
    fn take(&mut self, index: usize, left: f32, right: f32) {
        self.push(index, (left * left + right * right) / 2.0);
    }
}

/// The vocals themselves, both channels at [`RATE`].
#[derive(Default)]
struct Collected([Vec<f32>; 2]);

impl Settled for Collected {
    fn take(&mut self, _index: usize, left: f32, right: f32) {
        self.0[0].push(left);
        self.0[1].push(right);
    }
}

/// `demucs.apply`'s blending weights for a segment: rising from 1 to its
/// middle and falling back, scaled to a peak of 1.
fn triangle(length: usize) -> Vec<f32> {
    let half = length / 2;
    let peak = half.max(length - half) as f32;
    (0..length)
        .map(|n| if n < half { n + 1 } else { length - n } as f32 / peak)
        .collect()
}

/// Weighted vocals of overlapping segments from `base` on.
#[derive(Default)]
struct Blend {
    base: usize,
    sum: [Vec<f32>; 2],
    weight: Vec<f32>,
}

impl Blend {
    /// Add the first `valid` samples of a segment's `vocals` at `offset`.
    fn add(&mut self, offset: usize, valid: usize, vocals: [&[f32]; 2], weight: &[f32]) {
        let start = offset - self.base;
        let end = start + valid;
        if self.weight.len() < end {
            self.weight.resize(end, 0.0);
            for sum in &mut self.sum {
                sum.resize(end, 0.0);
            }
        }
        for n in 0..valid {
            self.weight[start + n] += weight[n];
            for (sum, channel) in self.sum.iter_mut().zip(&vocals) {
                sum[start + n] += weight[n] * channel[n];
            }
        }
    }

    /// Hand every sample before `until` to `settled` and forget it.
    fn settle(&mut self, until: usize, settled: &mut impl Settled) {
        let count = (until - self.base).min(self.weight.len());
        for n in 0..count {
            let weight = self.weight[n].max(f32::MIN_POSITIVE);
            let left = self.sum[0][n] / weight;
            let right = self.sum[1][n] / weight;
            settled.take(self.base + n, left, right);
        }
        self.weight.drain(..count);
        for sum in &mut self.sum {
            sum.drain(..count);
        }
        self.base += count;
    }
}

/// Mean power of every step of `step` samples.
struct StepMeter {
    length: usize,
    step: usize,
    power: Vec<f64>,
}

impl StepMeter {
    fn new(length: usize, step: usize) -> Self {
        Self {
            length,
            step,
            power: vec![0.0; length.div_ceil(step)],
        }
    }

    fn push(&mut self, sample: usize, power: f32) {
        if let Some(slot) = self.power.get_mut(sample / self.step) {
            *slot += f64::from(power);
        }
    }

    /// Each step's level in dBFS, at least [`SILENCE_DB`].
    fn levels(&self) -> Vec<f32> {
        self.power
            .iter()
            .enumerate()
            .map(|(index, power)| {
                let samples = self.step.min(self.length - index * self.step).max(1);
                let mean = power / samples as f64;
                ((10.0 * mean.max(1e-30).log10()) as f32).max(SILENCE_DB)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_blend_back_to_the_signal_they_came_from() {
        let length = 2 * SEGMENT + 12_345;
        let signal: Vec<f32> = (0..length)
            .map(|n| ((n % 97) as f32 / 97.0) - 0.5)
            .collect();
        let weight = triangle(SEGMENT);
        let step = 22_050;
        let mut meter = StepMeter::new(length, step);
        let mut blend = Blend::default();
        let mut offset = 0;
        while offset < length {
            let valid = (length - offset).min(SEGMENT);
            let piece = &signal[offset..offset + valid];
            blend.add(offset, valid, [piece, piece], &weight);
            blend.settle((offset + STRIDE).min(length), &mut meter);
            assert!(blend.weight.len() <= SEGMENT, "only one segment is kept");
            offset += STRIDE;
        }
        let levels = meter.levels();
        assert_eq!(levels.len(), length.div_ceil(step));
        let expected = {
            let power: f64 = signal[..step].iter().map(|v| f64::from(v * v)).sum();
            (10.0 * (power / step as f64).log10()) as f32
        };
        assert!(
            levels.iter().all(|level| (level - expected).abs() < 0.2),
            "{levels:?} vs {expected}"
        );
    }

    #[test]
    fn the_blend_weights_rise_and_fall_like_demucs() {
        let weight = triangle(8);
        assert_eq!(weight, vec![0.25, 0.5, 0.75, 1.0, 1.0, 0.75, 0.5, 0.25]);
    }
}
