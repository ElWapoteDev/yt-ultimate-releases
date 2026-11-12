//! Tempo control that preserves pitch and reports position in the source clock.
//!
//! At normal speed the samples pass straight through: SoundTouch is engaged
//! only when the speed first changes, and again only after a seek, so a
//! song played at 1× is exact and costs nothing. The control publishes the
//! source frame of every output frame, the clock a DJ mix aligns to.
use crate::{MediaError, Result};
use rodio::{source::SeekError, Source};
use std::{
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

/// Most channels a deck plays.
pub(crate) const MAX_DECK_CHANNELS: u16 = 16;
/// Frames SoundTouch processes per block.
const BLOCK_FRAMES: usize = 4096;
/// Samples between two publications of the position in seconds.
const POSITION_EVERY: usize = 256;

#[derive(Clone)]
pub(crate) struct TempoControl {
    speed: Arc<AtomicU32>,
    position: Arc<AtomicU64>,
    frame: Arc<AtomicU64>,
}
impl TempoControl {
    pub fn set_speed(&self, speed: f32) {
        self.speed.store(speed.to_bits(), Ordering::Relaxed);
    }
    pub fn speed(&self) -> f32 {
        f32::from_bits(self.speed.load(Ordering::Relaxed))
    }
    pub fn position(&self) -> f64 {
        self.position.load(Ordering::Relaxed) as f64 / 1_000_000_000.0
    }
    /// Source frame of the next output frame: exact while the speed has been
    /// 1 since the start or the last seek.
    pub fn frame(&self) -> u64 {
        self.frame.load(Ordering::Relaxed)
    }
}

pub(crate) struct TempoSource<S: Source<Item = f32>> {
    source: S,
    processor: soundtouch::SoundTouch,
    control: TempoControl,
    channels: u16,
    rate: u32,
    duration: Option<Duration>,
    buffer: Vec<f32>,
    input: Vec<f32>,
    cursor: usize,
    eof: bool,
    tempo: f64,
    buffer_tempo: f64,
    /// Samples pass through untouched: no speed change since the start or the
    /// last seek.
    direct: bool,
    /// Source samples behind the output so far (interleaved), fractional
    /// while SoundTouch stretches.
    consumed: f64,
    channel: u16,
    since_publish: usize,
}

impl<S: Source<Item = f32>> TempoSource<S> {
    pub fn new(source: S, speed: f32) -> Result<(Self, TempoControl)> {
        let channels = source.channels();
        let rate = source.sample_rate();
        let duration = source.total_duration();
        if !(1..=MAX_DECK_CHANNELS).contains(&channels) || rate == 0 {
            return Err(MediaError::Playback(
                "El audio tiene un formato no compatible con el control de velocidad.".into(),
            ));
        }
        let control = TempoControl {
            speed: Arc::new(AtomicU32::new(speed.to_bits())),
            position: Arc::new(AtomicU64::new(0)),
            frame: Arc::new(AtomicU64::new(0)),
        };
        let mut processor = soundtouch::SoundTouch::new();
        processor
            .set_channels(channels as u32)
            .set_sample_rate(rate)
            .set_tempo(speed as f64);
        Ok((
            Self {
                source,
                processor,
                control: control.clone(),
                channels,
                rate,
                duration,
                buffer: Vec::with_capacity(BLOCK_FRAMES * channels as usize),
                input: Vec::with_capacity(BLOCK_FRAMES * channels as usize),
                cursor: 0,
                eof: false,
                tempo: speed as f64,
                buffer_tempo: speed as f64,
                direct: speed == 1.0,
                consumed: 0.0,
                channel: 0,
                since_publish: 0,
            },
            control,
        ))
    }

    fn refill(&mut self) -> bool {
        self.buffer
            .resize(BLOCK_FRAMES * self.channels as usize, 0.0);
        loop {
            let frames = self
                .processor
                .receive_samples(&mut self.buffer, BLOCK_FRAMES);
            if frames > 0 {
                self.buffer.truncate(frames * self.channels as usize);
                self.cursor = 0;
                self.buffer_tempo = self.tempo;
                return true;
            }
            if self.eof {
                self.buffer.clear();
                return false;
            }
            let tempo = f64::from(self.control.speed());
            if tempo != self.tempo {
                self.processor.set_tempo(tempo);
                self.tempo = tempo;
            }
            self.input.clear();
            self.input.extend(
                self.source
                    .by_ref()
                    .take(BLOCK_FRAMES * self.channels as usize),
            );
            if self.input.is_empty() {
                self.processor.flush();
                self.eof = true;
            } else {
                self.processor
                    .put_samples(&self.input, self.input.len() / self.channels as usize);
            }
        }
    }

    /// One output sample that stands for `tempo` source samples.
    fn advance(&mut self, tempo: f64) {
        self.consumed += tempo;
        self.channel += 1;
        if self.channel == self.channels {
            self.channel = 0;
            let frame = (self.consumed / f64::from(self.channels)).round() as u64;
            self.control.frame.store(frame, Ordering::Relaxed);
        }
        self.since_publish += 1;
        if self.since_publish >= POSITION_EVERY {
            self.publish_position();
        }
    }

    fn publish_position(&mut self) {
        self.since_publish = 0;
        let seconds = self.consumed / (f64::from(self.rate) * f64::from(self.channels));
        let seconds = self
            .duration
            .map_or(seconds, |d| seconds.min(d.as_secs_f64()));
        self.control
            .position
            .store((seconds * 1_000_000_000.0) as u64, Ordering::Relaxed);
    }

    fn finish(&mut self) {
        if let Some(duration) = self.duration {
            self.control.position.store(
                duration.as_nanos().min(u64::MAX as u128) as u64,
                Ordering::Relaxed,
            );
        } else {
            self.publish_position();
        }
    }
}

impl<S: Source<Item = f32>> Iterator for TempoSource<S> {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        // SoundTouch takes over only on a frame boundary, never mid-frame.
        if self.direct && (self.channel != 0 || self.control.speed() == 1.0) {
            let Some(sample) = self.source.next() else {
                self.finish();
                return None;
            };
            self.advance(1.0);
            return Some(sample);
        }
        if self.direct {
            self.direct = false;
            self.tempo = f64::from(self.control.speed());
            self.processor.set_tempo(self.tempo);
            self.buffer.clear();
            self.cursor = 0;
        }
        if self.cursor >= self.buffer.len() && !self.refill() {
            self.finish();
            return None;
        }
        let sample = self.buffer[self.cursor];
        self.cursor += 1;
        self.advance(self.buffer_tempo);
        Some(sample)
    }
}
impl<S: Source<Item = f32>> Source for TempoSource<S> {
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
    fn try_seek(&mut self, position: Duration) -> std::result::Result<(), SeekError> {
        self.source.try_seek(position)?;
        self.processor.clear();
        self.buffer.clear();
        self.cursor = 0;
        self.eof = false;
        self.channel = 0;
        let frames = (position.as_secs_f64() * f64::from(self.rate)).round();
        self.consumed = frames * f64::from(self.channels);
        self.control.frame.store(frames as u64, Ordering::Relaxed);
        self.publish_position();
        self.tempo = f64::from(self.control.speed());
        self.processor.set_tempo(self.tempo);
        self.direct = self.tempo == 1.0;
        Ok(())
    }
}

/// `samples` (interleaved, `channels` at `rate`) as a deck plays them at
/// `speed`: the same time stretch, from the same first frame, so audio
/// stretched here lines up sample for sample with a deck that starts on that
/// frame at that speed. Fails for a layout the decks do not play.
pub fn stretch(samples: Vec<f32>, channels: u16, rate: u32, speed: f32) -> Result<Vec<f32>> {
    let source = rodio::buffer::SamplesBuffer::new(channels, rate, samples);
    let (stretched, _) = TempoSource::new(source, speed)?;
    Ok(stretched.collect())
}

/// Largest move [`transpose`] makes, in semitones.
pub const MAX_TRANSPOSE_SEMITONES: i8 = 12;

/// `samples` (interleaved, `channels` at `rate`) moved `semitones` up
/// (negative: down) at the same speed and the same length, every sound where
/// it was within a few milliseconds. For audio prepared ahead of time, off
/// the interface and audio threads: it processes the whole buffer. Fails for
/// a layout the decks do not play or a move beyond
/// [`MAX_TRANSPOSE_SEMITONES`].
pub fn transpose(samples: &[f32], channels: u16, rate: u32, semitones: i8) -> Result<Vec<f32>> {
    let width = usize::from(channels);
    if !(1..=MAX_DECK_CHANNELS).contains(&channels)
        || rate == 0
        || !samples.len().is_multiple_of(width)
        || semitones.abs() > MAX_TRANSPOSE_SEMITONES
    {
        return Err(MediaError::InvalidInput(
            "transpose: the audio or the move is not supported".into(),
        ));
    }
    if semitones == 0 {
        return Ok(samples.to_vec());
    }
    let mut processor = soundtouch::SoundTouch::new();
    processor
        .set_channels(u32::from(channels))
        .set_sample_rate(rate)
        .set_pitch_semitones(i32::from(semitones));
    let mut moved = Vec::with_capacity(samples.len());
    let mut block = vec![0.0_f32; BLOCK_FRAMES * width];
    let mut drain = |processor: &mut soundtouch::SoundTouch, moved: &mut Vec<f32>| loop {
        let frames = processor.receive_samples(&mut block, BLOCK_FRAMES);
        if frames == 0 {
            break;
        }
        moved.extend_from_slice(&block[..frames * width]);
    };
    for input in samples.chunks(BLOCK_FRAMES * width) {
        processor.put_samples(input, input.len() / width);
        drain(&mut processor, &mut moved);
    }
    processor.flush();
    drain(&mut processor, &mut moved);
    // Flushing pads the end: the result is exactly as long as the input.
    moved.resize(samples.len(), 0.0);
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transposed_audio_changes_pitch_and_keeps_its_place() {
        let rate = 44_100_usize;
        // Silence, then two seconds of a 440 Hz tone from the first second on.
        let input: Vec<f32> = (0..rate * 3)
            .flat_map(|n| {
                let tone = (std::f32::consts::TAU * 440.0 * n as f32 / rate as f32).sin() * 0.4;
                let sample = if n >= rate { tone } else { 0.0 };
                [sample, sample]
            })
            .collect();
        for (semitones, expected) in [(2_i8, 493.88_f64), (-1, 415.30)] {
            let output = transpose(&input, 2, rate as u32, semitones).unwrap();
            assert_eq!(output.len(), input.len());
            let left: Vec<f32> = output.chunks(2).map(|frame| frame[0]).collect();
            let body = &left[rate * 3 / 2..rate * 5 / 2];
            let crossings = body
                .windows(2)
                .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
                .count();
            let frequency = crossings as f64 * rate as f64 / body.len() as f64;
            assert!(
                (frequency - expected).abs() < 3.0,
                "{semitones} semitones: {frequency} Hz, {expected} expected"
            );
            let onset = left.iter().position(|sample| sample.abs() > 0.2).unwrap();
            let error_ms = (onset as f64 - rate as f64).abs() / rate as f64 * 1_000.0;
            assert!(
                error_ms < 10.0,
                "{semitones} semitones: the tone starts {error_ms} ms away"
            );
        }
        assert_eq!(transpose(&input, 2, rate as u32, 0).unwrap(), input);
        assert!(transpose(&input, 2, rate as u32, 13).is_err());
        assert!(transpose(&input[..3], 2, rate as u32, 1).is_err());
    }

    #[test]
    fn double_speed_preserves_pitch_and_source_clock() {
        let input: Vec<f32> = (0..96000)
            .map(|n| (std::f32::consts::TAU * 440.0 * n as f32 / 48000.0).sin() * 0.2)
            .collect();
        let (source, control) =
            TempoSource::new(rodio::buffer::SamplesBuffer::new(1, 48000, input), 2.0).unwrap();
        let output: Vec<f32> = source.collect();
        assert!((output.len() as i64 - 48000).abs() < 512);
        let body = &output[4096..output.len() - 4096];
        let crossings = body
            .windows(2)
            .filter(|s| s[0] <= 0.0 && s[1] > 0.0)
            .count();
        let frequency = crossings as f64 * 48000.0 / body.len() as f64;
        assert!(
            (430.0..450.0).contains(&frequency),
            "Pitch changed to {frequency} Hz"
        );
        assert!((control.position() - 2.0).abs() < 0.02);
    }

    #[test]
    fn normal_speed_passes_samples_through_with_an_exact_frame_clock() {
        let input: Vec<f32> = (0..2_000).map(|n| n as f32).collect();
        let (mut source, control) = TempoSource::new(
            rodio::buffer::SamplesBuffer::new(2, 44_100, input.clone()),
            1.0,
        )
        .unwrap();
        let first: Vec<f32> = source.by_ref().take(600).collect();
        assert_eq!(first, input[..600], "nothing is resampled at 1×");
        assert_eq!(control.frame(), 300);
        source
            .try_seek(Duration::from_secs_f64(500.0 / 44_100.0))
            .unwrap();
        assert_eq!(control.frame(), 500);
        assert_eq!(source.next(), Some(1_000.0));
    }

    #[test]
    fn a_speed_change_engages_the_stretch_on_a_frame_boundary() {
        let input: Vec<f32> = (0..96_000)
            .map(|n| (std::f32::consts::TAU * 220.0 * (n / 2) as f32 / 48_000.0).sin() * 0.2)
            .collect();
        let (mut source, control) =
            TempoSource::new(rodio::buffer::SamplesBuffer::new(2, 48_000, input), 1.0).unwrap();
        let _ = source.by_ref().take(1_001).count();
        control.set_speed(1.25);
        let rest = source.count();
        let expected = (96_000.0 - 1_002.0) / 1.25;
        assert!(
            (rest as f64 - expected).abs() < 2_048.0,
            "{rest} samples after the change, about {expected} expected"
        );
        assert!(
            (control.position() - 1.0).abs() < 0.02,
            "{}",
            control.position()
        );
    }

    /// A DJ mix matches tempos within ±6 %; the stretch must keep every beat
    /// where the new tempo puts it, with no lag or drift.
    #[test]
    fn stretched_audio_keeps_its_beats_where_the_tempo_puts_them() {
        let rate = 44_100_usize;
        let period = rate / 2;
        for speed in [0.94_f32, 0.97, 1.02, 1.06] {
            let mut input = vec![0.0_f32; rate * 20 * 2];
            for click in 1..40 {
                let at = click * period;
                input[at * 2..(at + 20) * 2].fill(0.9);
            }
            let (source, _) = TempoSource::new(
                rodio::buffer::SamplesBuffer::new(2, rate as u32, input),
                speed,
            )
            .unwrap();
            let output: Vec<f32> = source.collect();
            let mut clicks = Vec::new();
            for (frame, samples) in output.chunks(2).enumerate() {
                let apart = clicks.last().is_none_or(|last| frame - last > period / 4);
                if samples[0] > 0.45 && apart {
                    clicks.push(frame);
                }
            }
            assert_eq!(clicks.len(), 39, "speed {speed}");
            for (index, frame) in clicks.iter().enumerate() {
                let expected = ((index + 1) * period) as f64 / f64::from(speed);
                let error_ms = (*frame as f64 - expected).abs() / rate as f64 * 1_000.0;
                assert!(
                    error_ms < 5.0,
                    "speed {speed}, click {index}: {error_ms} ms"
                );
            }
        }
    }
}
