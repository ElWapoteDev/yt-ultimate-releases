//! How a song sounds, measured from its decoded samples on the scales of
//! [`AudioFeatures`]. The playback engine feeds an [`Analyzer`] while a song
//! plays; [`analyze_file`] measures a library file on a background thread.
//!
//! Samples are mixed to mono and cut into Hann-windowed frames of about 46 ms
//! (the nearest power of two: 2,048 samples at 44.1 and 48 kHz) that overlap
//! by half. Each frame contributes its RMS level, the centroid of its
//! magnitude spectrum, the positive spectral flux of its log-compressed
//! magnitudes, and a pitch-class profile built from its spectral peaks. Only
//! running sums outlive a frame, so memory is fixed when the analyzer is
//! created and pushing a sample never allocates or locks.

use crate::{decode_file, CancellationToken, MediaError, Result};
use rodio::Source;
use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::{f64::consts::FRAC_PI_2, ops::RangeInclusive, path::Path, sync::Arc, time::Duration};
use yt_core::{AudioFeatures, KeyEstimate, KeyMode, PitchClass};

/// Target frame length, in seconds; frames use the nearest power of two.
const FRAME_SECONDS: f64 = 0.046;
/// Accepted frame lengths, in samples (powers of two), for unusual rates.
const FRAME_LEN_RANGE: RangeInclusive<usize> = 256..=16_384;
/// A new frame starts every `frame length / FRAME_HOP_DIVISOR` samples, so
/// consecutive frames overlap by half.
const FRAME_HOP_DIVISOR: usize = 2;
/// Sample rates the analysis accepts, in Hz; others are clamped so a corrupt
/// stream header cannot inflate the buffers.
const SAMPLE_RATE_RANGE: RangeInclusive<u32> = 1..=768_000;
/// Largest accepted sample magnitude (+24 dBFS); louder samples are clamped.
const MAX_SAMPLE: f32 = 16.0;
/// Frames whose RMS level is below this, in dBFS, are silence: they are left
/// out of energy, dynamics, brightness and tonality.
const SILENCE_GATE_DBFS: f64 = -70.0;

/// Flux compares magnitudes compressed as ln(1 + FLUX_COMPRESSION · amplitude
/// / level), where level is the recent loudest frame RMS: bins 40 dB or more
/// below it count almost linearly, so noise barely moves the flux, and louder
/// bins logarithmically, so an onset counts the same at any playback level.
const FLUX_COMPRESSION: f32 = 100.0;
/// Release time of the recent-level follower, in seconds; its attack is
/// instant so an onset is compressed against the level that preceded it.
const LEVEL_RELEASE_SECONDS: f64 = 3.0;
/// An onset is a flux maximum within this distance on either side, in
/// seconds…
const ONSET_PEAK_RADIUS_SECONDS: f64 = 0.03;
/// …that exceeds the flux averaged over this distance on either side, in
/// seconds…
const ONSET_MEAN_RADIUS_SECONDS: f64 = 0.1;
/// …by at least this margin, in mean compressed-magnitude rise per bin…
const ONSET_MARGIN: f32 = 0.05;
/// …and comes at least this long after the previous onset, in seconds.
const ONSET_MIN_GAP_SECONDS: f64 = 0.05;

/// Half-width of the raised-cosine kernel that smooths the onset envelope,
/// in seconds, so a beat period that falls between two frame lags keeps its
/// autocorrelation peak instead of splitting it.
const BEAT_SMOOTHING_SECONDS: f64 = 0.05;
/// Centre of the log-normal tempo prior that settles octave ambiguity, in BPM.
const PREFERRED_TEMPO_BPM: f64 = 120.0;
/// Standard deviation of the tempo prior, in octaves.
const TEMPO_PRIOR_OCTAVES: f64 = 1.0;
/// Normalized autocorrelation the beat period needs for a tempo to be
/// reported; white noise stays below it after a few seconds.
const MIN_BEAT_CORRELATION: f64 = 0.2;
/// Standard deviation the smoothed onset envelope needs for a tempo to be
/// reported, in compressed-magnitude rise per bin; sustained tones and steady
/// noise stay below it.
const MIN_ONSET_DEVIATION: f64 = 0.005;
/// Shortest candidate beat period, in frames: a peak needs a neighbour on
/// each side.
const MIN_BEAT_LAG: usize = 2;
const SECONDS_PER_MINUTE: f64 = 60.0;

/// Spectral peaks inside this band, in Hz, build the pitch-class profile.
const CHROMA_BAND_HZ: RangeInclusive<f64> = 100.0..=5_000.0;
/// Peaks more than this far below a frame's strongest one, in dB, are ignored.
const CHROMA_PEAK_RANGE_DB: f32 = 40.0;
/// Reference tuning (A4), in Hz.
const A4_HZ: f64 = 440.0;
/// Pitch class of A, counting C as 0.
const A_PITCH_CLASS: i64 = 9;
/// Pitch classes (semitones) per octave.
const PITCH_CLASSES: usize = 12;
/// Krumhansl–Kessler major-key probe-tone ratings, tonic first.
const MAJOR_PROFILE: [f64; PITCH_CLASSES] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
/// Krumhansl–Kessler minor-key probe-tone ratings, tonic first.
const MINOR_PROFILE: [f64; PITCH_CLASSES] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];
/// Smallest magnitude taken to a logarithm, so empty bins stay finite.
const MAGNITUDE_FLOOR: f32 = 1e-12;

/// Decoded samples between two cancellation checks in [`analyze_file`].
const CANCEL_CHECK_SAMPLES: usize = 1 << 16;
/// Why a file without enough audio cannot be analyzed.
const TOO_SHORT: &str = "el audio es demasiado corto para analizarlo.";

/// Streaming analyzer; everything is preallocated in `new`.
///
/// Push channel-interleaved samples in playback order and read
/// [`Analyzer::features`] whenever a measurement is needed. Memory is fixed at
/// creation (roughly 100 KiB at 44.1 or 48 kHz), and [`Analyzer::push`] never
/// allocates, locks or blocks, so the analyzer can run on the audio thread.
pub struct Analyzer {
    sample_rate: u32,
    channels: u16,
    channel: u16,
    mix: f32,
    mono_samples: u64,
    frame: FrameBuffer,
    spectrum: Spectrum,
    level: Moments,
    centroid: Moments,
    onsets: Onsets,
    beat: Beat,
    pitch: PitchProfile,
}

impl Analyzer {
    /// Audio needed before [`Analyzer::features`] reports anything, in seconds.
    pub const MIN_SECONDS: f64 = 5.0;

    /// An analyzer for `channels` interleaved channels at `sample_rate` Hz.
    /// Sample rates outside 1 Hz–768 kHz are clamped and zero channels count
    /// as one, so a corrupt stream header yields meaningless numbers, never a
    /// panic or an oversized buffer.
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        let sample_rate = sample_rate.clamp(*SAMPLE_RATE_RANGE.start(), *SAMPLE_RATE_RANGE.end());
        let frame_len = frame_len_for(sample_rate);
        let hop = frame_len / FRAME_HOP_DIVISOR;
        let frame_period = hop as f64 / f64::from(sample_rate);
        let bin_hz = f64::from(sample_rate) / frame_len as f64;
        Self {
            sample_rate,
            channels: channels.max(1),
            channel: 0,
            mix: 0.0,
            mono_samples: 0,
            frame: FrameBuffer::new(frame_len, hop),
            spectrum: Spectrum::new(frame_len, frame_period, bin_hz),
            level: Moments::default(),
            centroid: Moments::default(),
            onsets: Onsets::new(frame_period),
            beat: Beat::new(frame_period),
            pitch: PitchProfile::new(frame_len, bin_hz),
        }
    }

    /// One interleaved sample. O(1) amortized: when a frame completes it runs one FFT.
    /// Never allocates, never locks. A non-finite sample counts as silence and
    /// samples beyond ±16 (+24 dBFS) are clamped.
    pub fn push(&mut self, sample: f32) {
        self.mix += if sample.is_finite() {
            sample.clamp(-MAX_SAMPLE, MAX_SAMPLE)
        } else {
            0.0
        };
        self.channel += 1;
        if self.channel < self.channels {
            return;
        }
        let mono = self.mix / f32::from(self.channels);
        self.channel = 0;
        self.mix = 0.0;
        self.mono_samples += 1;
        if self.frame.push(mono) {
            self.analyze_frame();
        }
    }

    /// Seconds of audio pushed so far (complete channel frames over the
    /// sample rate), including silence and audio pushed again after a seek.
    pub fn analyzed_seconds(&self) -> f64 {
        self.mono_samples as f64 / f64::from(self.sample_rate)
    }

    /// Features so far, or `None` before [`Analyzer::MIN_SECONDS`] of audio.
    ///
    /// Tempo (onset-envelope autocorrelation, octave settled by a perceptual
    /// prior) and mode (best of the 24 Krumhansl–Kessler keys) are computed
    /// from the accumulated state without allocating, in time proportional to
    /// the number of candidate beat periods. Every field is finite and on the
    /// scale documented on [`AudioFeatures`]; `analyzed_seconds` may be below
    /// [`AudioFeatures::MIN_ANALYZED_SECONDS`], and the caller decides when a
    /// measurement is worth storing.
    pub fn features(&mut self) -> Option<AudioFeatures> {
        let analyzed_seconds = self.analyzed_seconds();
        if analyzed_seconds < Self::MIN_SECONDS {
            return None;
        }
        let (energy_floor, energy_ceiling) = (
            *AudioFeatures::ENERGY_RANGE_DBFS.start(),
            *AudioFeatures::ENERGY_RANGE_DBFS.end(),
        );
        let (dark, bright) = (
            *AudioFeatures::BRIGHTNESS_RANGE_HZ.start(),
            *AudioFeatures::BRIGHTNESS_RANGE_HZ.end(),
        );
        Some(AudioFeatures {
            analyzed_seconds,
            energy: self.level.mean().map_or(0.0, |level| {
                unit_interval((level - energy_floor) / (energy_ceiling - energy_floor))
            }),
            dynamics: unit_interval(self.level.deviation() / AudioFeatures::FULL_DYNAMICS_DB),
            brightness: self.centroid.mean().map_or(0.0, |centroid| {
                unit_interval((centroid / dark).ln() / (bright / dark).ln())
            }),
            onset_rate: unit_interval(
                self.onsets.count as f64 / analyzed_seconds / AudioFeatures::FULL_ONSET_RATE,
            ),
            tempo_bpm: self.beat.tempo(),
            mode: self.pitch.mode(),
        })
    }

    /// The key that best fits the audio so far: the best of the 24
    /// Krumhansl–Kessler profiles for the same pitch-class profile `features`
    /// uses for `mode`. `None` before [`Self::MIN_SECONDS`] of audio or while
    /// nothing pitched has sounded.
    pub fn key(&self) -> Option<KeyEstimate> {
        if self.analyzed_seconds() < Self::MIN_SECONDS {
            return None;
        }
        self.pitch.key()
    }

    /// Forget the partial frame and the onset history after a discontinuity
    /// such as a seek, so no frame, flux or beat period spans it. Every
    /// accumulated statistic is kept.
    pub(crate) fn restart(&mut self) {
        self.frame.restart();
        self.spectrum.has_previous = false;
        self.onsets.restart();
        self.beat.restart();
    }

    fn analyze_frame(&mut self) {
        let mean_square = self.spectrum.transform(self.frame.oldest_first());
        let flux = self.spectrum.flux(mean_square);
        let level_dbfs = 10.0 * mean_square.log10();
        if level_dbfs >= SILENCE_GATE_DBFS {
            self.level.add(level_dbfs);
            if let Some(centroid) = self.spectrum.centroid() {
                self.centroid.add(centroid);
            }
            self.pitch.add_frame(&self.spectrum.magnitudes);
        }
        if let Some(strength) = self.onsets.push(flux) {
            self.beat.push(strength);
        }
    }
}

/// Decode `path`, start at `start` (fall back to 0 if seeking fails or
/// `start` is not before the end), and analyze up to `length` of audio.
///
/// Blocking and CPU-bound: call it from a background worker. A decoding
/// failure part-way keeps what was measured before it. Fails with
/// [`MediaError::Cancelled`] as soon as `cancel` fires (checked every 65,536
/// samples), with the decoder's error when the file cannot be opened, and
/// with [`MediaError::Processing`] when less than [`Analyzer::MIN_SECONDS`] of
/// audio could be decoded.
pub fn analyze_file(
    path: &Path,
    start: Duration,
    length: Duration,
    cancel: &CancellationToken,
) -> Result<AudioFeatures> {
    if cancel.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    let mut source = decode_file(path)?;
    let seek = !start.is_zero() && source.total_duration().is_none_or(|end| start < end);
    if seek && source.try_seek(start).is_err() {
        source = decode_file(path)?;
    }
    let mut analyzer = Analyzer::new(source.sample_rate(), source.channels());
    let samples = length.as_secs_f64()
        * f64::from(source.sample_rate())
        * f64::from(source.channels().max(1));
    // Float-to-integer casts saturate, so an unbounded length reads to the end.
    measure(
        &mut analyzer,
        source.by_ref().take(samples as usize),
        cancel,
    )?;
    analyzer.features().ok_or_else(|| {
        MediaError::Processing(source.decoding_error().unwrap_or_else(|| TOO_SHORT.into()))
    })
}

/// Push every sample of `samples`, stopping early when `cancel` fires.
fn measure(
    analyzer: &mut Analyzer,
    samples: impl Iterator<Item = f32>,
    cancel: &CancellationToken,
) -> Result<()> {
    for (index, sample) in samples.enumerate() {
        if index % CANCEL_CHECK_SAMPLES == 0 && cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        analyzer.push(sample);
    }
    Ok(())
}

/// The power of two nearest (in octaves) to `FRAME_SECONDS` at `sample_rate`.
fn frame_len_for(sample_rate: u32) -> usize {
    let nearest = (FRAME_SECONDS * f64::from(sample_rate))
        .log2()
        .round()
        .exp2();
    nearest.clamp(
        *FRAME_LEN_RANGE.start() as f64,
        *FRAME_LEN_RANGE.end() as f64,
    ) as usize
}

/// Whole frames closest to `seconds`.
fn frames_in(seconds: f64, frame_period: f64) -> usize {
    (seconds / frame_period).round() as usize
}

/// `value` clamped to [0, 1]; NaN reads as 0.
fn unit_interval(value: f64) -> f64 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

/// Offset, within ±0.5 samples, of the vertex of the parabola through three
/// equally spaced values around a local maximum.
fn vertex_offset(left: f64, center: f64, right: f64) -> f64 {
    let curvature = left - 2.0 * center + right;
    let offset = 0.5 * (left - right) / curvature;
    if curvature < 0.0 && offset.is_finite() {
        offset.clamp(-0.5, 0.5)
    } else {
        0.0
    }
}

/// Running mean and variance (Welford).
#[derive(Default)]
struct Moments {
    count: u64,
    mean: f64,
    squares: f64,
}

impl Moments {
    fn add(&mut self, value: f64) {
        self.count += 1;
        let delta = value - self.mean;
        self.mean += delta / self.count as f64;
        self.squares += delta * (value - self.mean);
    }

    fn mean(&self) -> Option<f64> {
        (self.count > 0).then_some(self.mean)
    }

    /// Population standard deviation; 0 below two values.
    fn deviation(&self) -> f64 {
        if self.count < 2 {
            0.0
        } else {
            (self.squares / self.count as f64).sqrt()
        }
    }
}

/// The latest frame of mono samples and the countdown to the next frame.
struct FrameBuffer {
    samples: Vec<f32>,
    next: usize,
    until_frame: usize,
    hop: usize,
}

impl FrameBuffer {
    fn new(len: usize, hop: usize) -> Self {
        Self {
            samples: vec![0.0; len],
            next: 0,
            until_frame: len,
            hop,
        }
    }

    /// Store one sample; true when a frame is complete.
    fn push(&mut self, sample: f32) -> bool {
        if let Some(slot) = self.samples.get_mut(self.next) {
            *slot = sample;
        }
        self.next += 1;
        if self.next == self.samples.len() {
            self.next = 0;
        }
        self.until_frame -= 1;
        if self.until_frame > 0 {
            return false;
        }
        self.until_frame = self.hop;
        true
    }

    fn oldest_first(&self) -> impl Iterator<Item = f32> + '_ {
        let (newer, older) = self.samples.split_at(self.next);
        older.iter().chain(newer).copied()
    }

    /// The next frame holds only samples pushed from now on.
    fn restart(&mut self) {
        self.until_frame = self.samples.len();
    }
}

/// Window, FFT and the per-bin state that outlives one frame.
struct Spectrum {
    window: Vec<f32>,
    /// Makes a full-scale sine read amplitude 1 at its bin.
    amplitude_scale: f32,
    fft: Arc<dyn Fft<f32>>,
    buffer: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    magnitudes: Vec<f32>,
    compressed: Vec<f32>,
    has_previous: bool,
    bin_hz: f64,
    /// Recent loudest frame power, released by `release` per frame and never
    /// below `silence_power`.
    reference_power: f64,
    release: f64,
    silence_power: f64,
}

impl Spectrum {
    fn new(len: usize, frame_period: f64, bin_hz: f64) -> Self {
        let fft = FftPlanner::new().plan_fft_forward(len);
        let window: Vec<f32> = (0..len)
            .map(|index| {
                let phase = std::f64::consts::TAU * index as f64 / len as f64;
                (0.5 - 0.5 * phase.cos()) as f32
            })
            .collect();
        let window_sum: f32 = window.iter().sum();
        let silence_power = 10f64.powf(SILENCE_GATE_DBFS / 10.0);
        Self {
            amplitude_scale: 2.0 / window_sum,
            window,
            scratch: vec![Complex::default(); fft.get_inplace_scratch_len()],
            fft,
            buffer: vec![Complex::default(); len],
            magnitudes: vec![0.0; len / 2 + 1],
            compressed: vec![0.0; len / 2 + 1],
            has_previous: false,
            bin_hz,
            reference_power: silence_power,
            release: (-frame_period / LEVEL_RELEASE_SECONDS).exp(),
            silence_power,
        }
    }

    /// Window and transform one frame into `magnitudes`; returns the frame's
    /// mean square.
    fn transform(&mut self, samples: impl Iterator<Item = f32>) -> f64 {
        let mut energy = 0.0;
        for ((slot, sample), weight) in self.buffer.iter_mut().zip(samples).zip(&self.window) {
            energy += f64::from(sample) * f64::from(sample);
            *slot = Complex::new(sample * weight, 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.buffer, &mut self.scratch);
        for (magnitude, bin) in self.magnitudes.iter_mut().zip(&self.buffer) {
            *magnitude = bin.norm_sqr().sqrt() * self.amplitude_scale;
        }
        energy / self.buffer.len() as f64
    }

    /// Mean positive change of the compressed magnitudes since the previous
    /// frame (0 for the first frame after a start or seek).
    fn flux(&mut self, mean_square: f64) -> f32 {
        let reference = self.reference_power.sqrt() as f32;
        self.reference_power = (self.reference_power * self.release)
            .max(mean_square)
            .max(self.silence_power);
        let gain = FLUX_COMPRESSION / reference;
        let mut rise = 0.0;
        let bins = self.compressed.iter_mut().zip(&self.magnitudes).skip(1);
        let count = bins.len();
        for (previous, magnitude) in bins {
            let compressed = (gain * magnitude).ln_1p();
            rise += (compressed - *previous).max(0.0);
            *previous = compressed;
        }
        let flux = if self.has_previous && count > 0 {
            rise / count as f32
        } else {
            0.0
        };
        self.has_previous = true;
        flux
    }

    /// Magnitude-weighted mean frequency, in Hz, of every bin above DC.
    fn centroid(&self) -> Option<f64> {
        let (mut weighted, mut total) = (0.0, 0.0);
        for (bin, magnitude) in self.magnitudes.iter().enumerate().skip(1) {
            weighted += bin as f64 * f64::from(*magnitude);
            total += f64::from(*magnitude);
        }
        (total > 0.0).then(|| weighted / total * self.bin_hz)
    }
}

/// Adaptive-threshold onset picking over the spectral flux.
struct Onsets {
    /// The latest `2 · mean radius + 1` flux values, oldest first; the
    /// candidate is the middle one.
    flux: Vec<f32>,
    filled: usize,
    peak_radius: usize,
    min_gap: usize,
    since_onset: usize,
    count: u64,
}

impl Onsets {
    fn new(frame_period: f64) -> Self {
        let peak_radius = frames_in(ONSET_PEAK_RADIUS_SECONDS, frame_period).max(1);
        let mean_radius = frames_in(ONSET_MEAN_RADIUS_SECONDS, frame_period).max(peak_radius);
        Self {
            flux: vec![0.0; 2 * mean_radius + 1],
            filled: 0,
            peak_radius,
            min_gap: frames_in(ONSET_MIN_GAP_SECONDS, frame_period).max(1),
            since_onset: usize::MAX,
            count: 0,
        }
    }

    /// Add one frame's flux. Once the window is full, decide whether its
    /// middle frame is an onset and return that frame's onset strength (its
    /// flux above the local mean, never negative).
    fn push(&mut self, flux: f32) -> Option<f32> {
        self.flux.copy_within(1.., 0);
        *self.flux.last_mut()? = flux;
        self.filled = (self.filled + 1).min(self.flux.len());
        if self.filled < self.flux.len() {
            return None;
        }
        let middle = self.flux.len() / 2;
        let center = self.flux[middle];
        let mean = self.flux.iter().sum::<f32>() / self.flux.len() as f32;
        let earlier = &self.flux[middle - self.peak_radius..middle];
        let later = &self.flux[middle + 1..=middle + self.peak_radius];
        let is_peak = earlier.iter().all(|value| center >= *value)
            && later.iter().all(|value| center > *value);
        self.since_onset = self.since_onset.saturating_add(1);
        if is_peak && center >= mean + ONSET_MARGIN && self.since_onset >= self.min_gap {
            self.count += 1;
            self.since_onset = 0;
        }
        Some((center - mean).max(0.0))
    }

    fn restart(&mut self) {
        self.filled = 0;
        self.since_onset = usize::MAX;
    }
}

/// Autocorrelation of the smoothed onset envelope, accumulated over every
/// analyzed frame for each candidate beat period.
struct Beat {
    kernel: Vec<f32>,
    /// The latest `kernel.len()` onset strengths, oldest first.
    recent: Vec<f32>,
    /// The latest smoothed envelope values, oldest first; one per lag.
    history: Vec<f64>,
    filled: usize,
    /// Sum of envelope products and their count, indexed by lag in frames.
    products: Vec<f64>,
    pairs: Vec<u64>,
    sum: f64,
    /// Candidate beat periods, in frames.
    lags: RangeInclusive<usize>,
    correlation: Vec<f64>,
    frame_period: f64,
}

impl Beat {
    fn new(frame_period: f64) -> Self {
        let radius = frames_in(BEAT_SMOOTHING_SECONDS, frame_period);
        let weights: Vec<f32> = (0..=2 * radius)
            .map(|tap| {
                let distance = tap.abs_diff(radius) as f64 / (radius + 1) as f64;
                (FRAC_PI_2 * distance).cos().powi(2) as f32
            })
            .collect();
        let total: f32 = weights.iter().sum();
        let kernel: Vec<f32> = weights.iter().map(|weight| weight / total).collect();
        let lag_for = |bpm: f64| SECONDS_PER_MINUTE / bpm / frame_period;
        let shortest =
            (lag_for(*AudioFeatures::TEMPO_RANGE.end()).ceil() as usize).max(MIN_BEAT_LAG);
        let longest = lag_for(*AudioFeatures::TEMPO_RANGE.start()).floor() as usize;
        // Lags 0 through one past the longest period: interpolating a peak
        // needs its right neighbour.
        let lag_count = longest.max(shortest) + 2;
        Self {
            recent: vec![0.0; kernel.len()],
            kernel,
            history: vec![0.0; lag_count],
            filled: 0,
            products: vec![0.0; lag_count],
            pairs: vec![0; lag_count],
            sum: 0.0,
            lags: shortest..=longest,
            correlation: vec![0.0; lag_count],
            frame_period,
        }
    }

    fn push(&mut self, strength: f32) {
        self.recent.copy_within(1.., 0);
        if let Some(last) = self.recent.last_mut() {
            *last = strength;
        }
        let value = f64::from(
            self.kernel
                .iter()
                .zip(&self.recent)
                .map(|(weight, strength)| weight * strength)
                .sum::<f32>(),
        );
        self.history.copy_within(1.., 0);
        if let Some(last) = self.history.last_mut() {
            *last = value;
        }
        self.filled = (self.filled + 1).min(self.history.len());
        let lagged = self.history.iter().rev().take(self.filled);
        for ((product, pairs), past) in self.products.iter_mut().zip(&mut self.pairs).zip(lagged) {
            *product += value * past;
            *pairs += 1;
        }
        self.sum += value;
    }

    /// No beat period spans a discontinuity.
    fn restart(&mut self) {
        self.recent.fill(0.0);
        self.filled = 0;
    }

    /// The autocorrelation peak that best matches the tempo prior, refined
    /// between frame lags, or 0 without a stable beat inside the tempo range.
    fn tempo(&mut self) -> f64 {
        let frames = self.pairs.first().copied().unwrap_or(0);
        let energy = self.products.first().copied().unwrap_or(0.0);
        if frames == 0 {
            return 0.0;
        }
        let mean = self.sum / frames as f64;
        let variance = energy / frames as f64 - mean * mean;
        // `max` also maps a NaN variance to 0.
        if variance.max(0.0).sqrt() < MIN_ONSET_DEVIATION {
            return 0.0;
        }
        let rows = self.products.iter().zip(&self.pairs);
        for (correlation, (product, pairs)) in self.correlation.iter_mut().zip(rows) {
            *correlation = match *pairs {
                0 => 0.0,
                pairs => (product / pairs as f64 - mean * mean) / variance,
            };
        }
        let frame_period = self.frame_period;
        let best = self
            .correlation
            .windows(3)
            .enumerate()
            .map(|(index, window)| (index + 1, window))
            .filter(|(lag, _)| self.lags.contains(lag))
            .filter_map(|(lag, window)| match *window {
                [left, center, right] if center > left && center >= right => {
                    let bpm = SECONDS_PER_MINUTE / (lag as f64 * frame_period);
                    Some((lag, [left, center, right], center * tempo_prior(bpm)))
                }
                _ => None,
            })
            .max_by(|first, second| first.2.total_cmp(&second.2));
        let Some((lag, [left, center, right], _)) = best else {
            return 0.0;
        };
        if center < MIN_BEAT_CORRELATION {
            return 0.0;
        }
        let period = (lag as f64 + vertex_offset(left, center, right)) * frame_period;
        let bpm = SECONDS_PER_MINUTE / period;
        if AudioFeatures::TEMPO_RANGE.contains(&bpm) {
            bpm
        } else {
            0.0
        }
    }
}

/// Weight of a tempo under the log-normal perceptual prior (1 at its centre).
fn tempo_prior(bpm: f64) -> f64 {
    let octaves = (bpm / PREFERRED_TEMPO_BPM).log2() / TEMPO_PRIOR_OCTAVES;
    (-0.5 * octaves * octaves).exp()
}

/// Pitch-class profile from interpolated spectral peaks; each audible frame
/// adds a unit of weight, so it measures how long each pitch class sounds.
struct PitchProfile {
    /// FFT bins searched for peaks; never DC or the last bin, so every bin
    /// has a neighbour on each side.
    bins: RangeInclusive<usize>,
    bin_hz: f64,
    peak_floor: f32,
    chroma: [f64; PITCH_CLASSES],
}

impl PitchProfile {
    fn new(frame_len: usize, bin_hz: f64) -> Self {
        let first = ((CHROMA_BAND_HZ.start() / bin_hz).ceil() as usize).max(1);
        let last = ((CHROMA_BAND_HZ.end() / bin_hz).floor() as usize).min(frame_len / 2 - 1);
        Self {
            bins: first..=last,
            bin_hz,
            peak_floor: 10f32.powf(-CHROMA_PEAK_RANGE_DB / 20.0),
            chroma: [0.0; PITCH_CLASSES],
        }
    }

    fn add_frame(&mut self, magnitudes: &[f32]) {
        let (first, last) = (*self.bins.start(), *self.bins.end());
        let Some(span) = magnitudes.get(first - 1..=last + 1) else {
            return;
        };
        let strongest = span.iter().copied().fold(0.0, f32::max);
        let floor = strongest * self.peak_floor;
        let mut frame = [0.0; PITCH_CLASSES];
        for (offset, window) in span.windows(3).enumerate() {
            let [left, center, right] = *window else {
                continue;
            };
            if center > floor && center > left && center >= right {
                let log = |magnitude: f32| f64::from(magnitude.max(MAGNITUDE_FLOOR).ln());
                let bin =
                    (first + offset) as f64 + vertex_offset(log(left), log(center), log(right));
                frame[pitch_class(bin * self.bin_hz)] += f64::from(center);
            }
        }
        let total: f64 = frame.iter().sum();
        if total > 0.0 {
            for (sum, weight) in self.chroma.iter_mut().zip(frame) {
                *sum += weight / total;
            }
        }
    }

    /// Correlation with the best major key minus the best minor key, in
    /// [-1, 1]; 0 without a pitch profile.
    fn mode(&self) -> f64 {
        let Some(((_, major), (_, minor))) = self.best_keys() else {
            return 0.0;
        };
        let mode = major - minor;
        if mode.is_finite() {
            mode.clamp(-1.0, 1.0)
        } else {
            0.0
        }
    }

    /// The best of the 24 Krumhansl–Kessler keys; `None` without a pitch
    /// profile.
    fn key(&self) -> Option<KeyEstimate> {
        let ((major_tonic, major), (minor_tonic, minor)) = self.best_keys()?;
        let (tonic, mode, correlation) = if major >= minor {
            (major_tonic, KeyMode::Major, major)
        } else {
            (minor_tonic, KeyMode::Minor, minor)
        };
        if !correlation.is_finite() {
            return None;
        }
        Some(KeyEstimate {
            tonic: PitchClass::new(tonic as u8).ok()?,
            mode,
            confidence: correlation.clamp(0.0, 1.0) as f32,
        })
    }

    /// Best major and best minor tonic with their correlations, or `None`
    /// while the profile is flat (silence, unpitched sound).
    fn best_keys(&self) -> Option<((usize, f64), (usize, f64))> {
        let mean = self.chroma.iter().sum::<f64>() / PITCH_CLASSES as f64;
        let spread: f64 = self.chroma.iter().map(|value| (value - mean).powi(2)).sum();
        if !spread.is_normal() {
            return None;
        }
        let best = |profile: &[f64; PITCH_CLASSES]| {
            (0..PITCH_CLASSES)
                .map(|tonic| (tonic, correlation(&self.chroma, profile, tonic)))
                .fold((0, f64::NEG_INFINITY), |best, candidate| {
                    if candidate.1 > best.1 {
                        candidate
                    } else {
                        best
                    }
                })
        };
        Some((best(&MAJOR_PROFILE), best(&MINOR_PROFILE)))
    }
}

/// Pitch class of `frequency` (C = 0) in equal temperament around `A4_HZ`.
fn pitch_class(frequency: f64) -> usize {
    let semitones = (PITCH_CLASSES as f64 * (frequency / A4_HZ).log2()).round() as i64;
    (semitones + A_PITCH_CLASS).rem_euclid(PITCH_CLASSES as i64) as usize
}

/// Pearson correlation of `chroma` with `profile` rotated to `tonic`.
fn correlation(chroma: &[f64; PITCH_CLASSES], profile: &[f64; PITCH_CLASSES], tonic: usize) -> f64 {
    let chroma_mean = chroma.iter().sum::<f64>() / PITCH_CLASSES as f64;
    let profile_mean = profile.iter().sum::<f64>() / PITCH_CLASSES as f64;
    let rotated = profile
        .iter()
        .cycle()
        .skip((PITCH_CLASSES - tonic % PITCH_CLASSES) % PITCH_CLASSES);
    let (mut covariance, mut chroma_spread, mut profile_spread) = (0.0, 0.0, 0.0);
    for (value, rating) in chroma.iter().zip(rotated) {
        let (x, y) = (value - chroma_mean, rating - profile_mean);
        covariance += x * y;
        chroma_spread += x * x;
        profile_spread += y * y;
    }
    covariance / (chroma_spread * profile_spread).sqrt()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::f64::consts::{SQRT_2, TAU};

    const RATE: u32 = 44_100;

    /// A mono 16-bit PCM WAV file.
    pub(crate) fn pcm16_wav(sample_rate: u32, samples: &[i16]) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let mut wav = Vec::with_capacity(44 + samples.len() * 2);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        wav
    }

    /// Deterministic white noise in [-1, 1).
    struct Noise(u64);

    impl Noise {
        fn sample(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 40) as f32 / (1u64 << 23) as f32 - 1.0
        }
    }

    fn times(seconds: f64) -> impl Iterator<Item = f64> {
        (0..(seconds * f64::from(RATE)) as usize).map(|index| index as f64 / f64::from(RATE))
    }

    fn analyze(channels: u16, samples: impl IntoIterator<Item = f32>) -> Analyzer {
        let mut analyzer = Analyzer::new(RATE, channels);
        for sample in samples {
            for _ in 0..channels {
                analyzer.push(sample);
            }
        }
        analyzer
    }

    fn features(samples: impl IntoIterator<Item = f32>) -> AudioFeatures {
        analyze(1, samples).features().expect("long enough")
    }

    fn tones(frequencies: &[f64], amplitude: f64, seconds: f64) -> impl Iterator<Item = f32> + '_ {
        times(seconds).map(move |time| {
            frequencies
                .iter()
                .map(|frequency| amplitude * (TAU * frequency * time).sin())
                .sum::<f64>() as f32
        })
    }

    /// Decaying noise bursts, one per beat.
    fn clicks(bpm: f64, seconds: f64) -> Vec<f32> {
        const DECAY_SECONDS: f64 = 0.004;
        let period = SECONDS_PER_MINUTE / bpm;
        let mut noise = Noise(7);
        times(seconds)
            .map(|time| noise.sample() * (0.5 * (-(time % period) / DECAY_SECONDS).exp()) as f32)
            .collect()
    }

    /// Amplitude of a sine whose RMS level is `dbfs`.
    fn sine_amplitude(dbfs: f64) -> f64 {
        10f64.powf(dbfs / 20.0) * SQRT_2
    }

    #[test]
    fn brighter_tones_score_higher_brightness() {
        let low = features(tones(&[200.0], 0.25, 8.0));
        let high = features(tones(&[3_000.0], 0.25, 8.0));
        let expected = |hz: f64| (hz / 100.0).ln() / 80f64.ln();
        assert!(high.brightness > low.brightness + 0.5, "{low:?} {high:?}");
        assert!((low.brightness - expected(200.0)).abs() < 0.03, "{low:?}");
        assert!(
            (high.brightness - expected(3_000.0)).abs() < 0.03,
            "{high:?}"
        );
    }

    #[test]
    fn energy_maps_rms_level_linearly_and_steady_tones_have_no_dynamics() {
        let loud = features(tones(&[1_000.0], sine_amplitude(-6.0), 8.0));
        let quiet = features(tones(&[1_000.0], sine_amplitude(-30.0), 8.0));
        assert!(loud.energy > quiet.energy);
        assert!((loud.energy - 0.9).abs() < 0.01, "{loud:?}");
        assert!((quiet.energy - 0.5).abs() < 0.01, "{quiet:?}");
        assert!(loud.dynamics < 0.01 && quiet.dynamics < 0.01);
        assert_eq!(loud.tempo_bpm, 0.0);
        assert!(loud.onset_rate < 0.02, "{loud:?}");
    }

    #[test]
    fn click_trains_report_their_tempo_and_onset_rate() {
        for bpm in [90.0, 120.0, 150.0] {
            let measured = features(clicks(bpm, 30.0));
            assert!(
                (measured.tempo_bpm - bpm).abs() <= 4.0,
                "{bpm}: {measured:?}"
            );
            let onsets_per_second = bpm / SECONDS_PER_MINUTE;
            assert!(
                (measured.onset_rate - onsets_per_second / AudioFeatures::FULL_ONSET_RATE).abs()
                    < 0.02,
                "{bpm}: {measured:?}"
            );
        }
    }

    #[test]
    fn major_and_minor_triads_lean_their_way() {
        let major = features(tones(&[523.25, 659.26, 783.99], 0.1, 10.0));
        let minor = features(tones(&[523.25, 622.25, 783.99], 0.1, 10.0));
        assert!(major.mode > 0.0, "{major:?}");
        assert!(minor.mode < 0.0, "{minor:?}");
        assert!(major.mode - minor.mode > 0.2, "{major:?} {minor:?}");
    }

    #[test]
    fn triads_report_their_tonic_and_mode() {
        let c_major = analyze(1, tones(&[523.25, 659.26, 783.99], 0.1, 10.0))
            .key()
            .unwrap();
        assert_eq!((c_major.tonic.value(), c_major.mode), (0, KeyMode::Major));
        let a_minor = analyze(1, tones(&[440.0, 523.25, 659.26], 0.1, 10.0))
            .key()
            .unwrap();
        assert_eq!((a_minor.tonic.value(), a_minor.mode), (9, KeyMode::Minor));
        assert!((0.0..=1.0).contains(&a_minor.confidence));
        assert!(analyze(1, std::iter::repeat_n(0.0, 10 * RATE as usize))
            .key()
            .is_none());
        assert!(
            analyze(1, tones(&[440.0], 0.1, 2.0)).key().is_none(),
            "too short"
        );
    }

    #[test]
    fn silence_measures_zero_without_nan() {
        let silence = analyze(2, std::iter::repeat_n(0.0, 25 * RATE as usize))
            .features()
            .expect("long enough");
        assert!((silence.analyzed_seconds - 25.0).abs() < 1e-9);
        for value in [
            silence.energy,
            silence.dynamics,
            silence.brightness,
            silence.onset_rate,
            silence.tempo_bpm,
            silence.mode,
        ] {
            assert_eq!(value, 0.0, "{silence:?}");
        }
        assert!(silence.validate().is_ok());
    }

    #[test]
    fn noise_has_no_beat_and_corrupt_samples_stay_finite() {
        let mut noise = Noise(3);
        let measured = features((0..20 * RATE).map(|_| noise.sample() * 0.1));
        assert_eq!(measured.tempo_bpm, 0.0, "{measured:?}");
        assert!(measured.onset_rate < 0.02, "{measured:?}");
        assert!(measured.brightness > 0.9, "{measured:?}");
        let corrupt = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e30, -1e30];
        let mixed = tones(&[440.0], 0.25, 21.0)
            .enumerate()
            .map(|(index, sample)| corrupt.get(index % 1_000).copied().unwrap_or(sample));
        let measured = features(mixed);
        assert!(measured.validate().is_ok(), "{measured:?}");
    }

    #[test]
    fn features_wait_for_the_minimum_duration() {
        let mut analyzer = analyze(2, tones(&[440.0], 0.25, Analyzer::MIN_SECONDS - 0.5));
        assert!(analyzer.features().is_none());
        for sample in tones(&[440.0], 0.25, 1.0) {
            analyzer.push(sample);
            analyzer.push(sample);
        }
        let measured = analyzer.features().expect("past the minimum");
        assert!((measured.analyzed_seconds - (Analyzer::MIN_SECONDS + 0.5)).abs() < 1e-3);
    }

    #[test]
    fn restarts_keep_statistics_and_add_no_onsets() {
        let steady = features(tones(&[440.0], 0.25, 10.0));
        let mut analyzer = Analyzer::new(RATE, 1);
        for (index, sample) in tones(&[440.0], 0.25, 10.0).enumerate() {
            if index % 16_411 == 0 {
                analyzer.restart();
            }
            analyzer.push(sample);
        }
        let restarted = analyzer.features().expect("long enough");
        assert_eq!(restarted.onset_rate, 0.0, "{restarted:?}");
        assert!((restarted.analyzed_seconds - steady.analyzed_seconds).abs() < 1e-9);
        assert!(
            (restarted.energy - steady.energy).abs() < 1e-3,
            "{restarted:?}"
        );
        assert!(
            (restarted.brightness - steady.brightness).abs() < 1e-3,
            "{restarted:?}"
        );
    }

    #[test]
    fn files_are_analyzed_from_the_requested_start_for_the_requested_length() {
        const WAV_RATE: u32 = 22_050;
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("tone.wav");
        let samples: Vec<i16> = (0..30 * WAV_RATE)
            .map(|index| {
                let time = f64::from(index) / f64::from(WAV_RATE);
                (0.25 * (TAU * 440.0 * time).sin() * f64::from(i16::MAX)) as i16
            })
            .collect();
        std::fs::write(&path, pcm16_wav(WAV_RATE, &samples)).unwrap();
        let cancel = CancellationToken::new();
        let seconds = |start: u64, length: Duration| {
            analyze_file(&path, Duration::from_secs(start), length, &cancel)
                .unwrap()
                .analyzed_seconds
        };
        assert!((seconds(10, Duration::from_secs(15)) - 15.0).abs() < 0.01);
        assert!((seconds(20, Duration::from_secs(15)) - 10.0).abs() < 0.01);
        assert!((seconds(40, Duration::from_secs(15)) - 15.0).abs() < 0.01);
        assert!((seconds(0, Duration::MAX) - 30.0).abs() < 0.01);
        assert!(matches!(
            analyze_file(&path, Duration::ZERO, Duration::from_secs(2), &cancel),
            Err(MediaError::Processing(_))
        ));
        cancel.cancel();
        assert!(matches!(
            analyze_file(&path, Duration::ZERO, Duration::MAX, &cancel),
            Err(MediaError::Cancelled)
        ));
    }

    #[test]
    fn measuring_stops_when_cancelled_mid_stream() {
        let cancel = CancellationToken::new();
        let mut analyzer = Analyzer::new(RATE, 2);
        let endless = std::iter::repeat(0.1).enumerate().map(|(index, sample)| {
            if index == 10 * CANCEL_CHECK_SAMPLES {
                cancel.cancel();
            }
            sample
        });
        assert!(matches!(
            measure(&mut analyzer, endless, &cancel),
            Err(MediaError::Cancelled)
        ));
        assert!(analyzer.analyzed_seconds() > 0.0);
    }
}
