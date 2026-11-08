//! Tempo control that preserves pitch and reports position in the source clock.
use crate::{MediaError, Result};
use rodio::{source::SeekError, Source};
use std::{
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

#[derive(Clone)]
pub(crate) struct TempoControl {
    speed: Arc<AtomicU32>,
    position: Arc<AtomicU64>,
}
impl TempoControl {
    pub fn set_speed(&self, speed: f32) {
        self.speed.store(speed.to_bits(), Ordering::Relaxed);
    }
    pub fn position(&self) -> f64 {
        self.position.load(Ordering::Relaxed) as f64 / 1_000_000_000.0
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
    cursor: usize,
    eof: bool,
    tempo: f64,
    buffer_tempo: f64,
    position: f64,
}

impl<S: Source<Item = f32>> TempoSource<S> {
    pub fn new(source: S, speed: f32) -> Result<(Self, TempoControl)> {
        let channels = source.channels();
        let rate = source.sample_rate();
        let duration = source.total_duration();
        if !(1..=16).contains(&channels) || rate == 0 {
            return Err(MediaError::Playback(
                "El audio tiene un formato no compatible con el control de velocidad.".into(),
            ));
        }
        let control = TempoControl {
            speed: Arc::new(AtomicU32::new(speed.to_bits())),
            position: Arc::new(AtomicU64::new(0)),
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
                buffer: Vec::new(),
                cursor: 0,
                eof: false,
                tempo: speed as f64,
                buffer_tempo: speed as f64,
                position: 0.0,
            },
            control,
        ))
    }
    fn refill(&mut self) -> bool {
        self.buffer.resize(4096 * self.channels as usize, 0.0);
        loop {
            let frames = self.processor.receive_samples(&mut self.buffer, 4096);
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
            let tempo = f32::from_bits(self.control.speed.load(Ordering::Relaxed)) as f64;
            if tempo != self.tempo {
                self.processor.set_tempo(tempo);
                self.tempo = tempo;
            }
            let input: Vec<f32> = self
                .source
                .by_ref()
                .take(4096 * self.channels as usize)
                .collect();
            if input.is_empty() {
                self.processor.flush();
                self.eof = true;
            } else {
                self.processor
                    .put_samples(&input, input.len() / self.channels as usize);
            }
        }
    }
}
impl<S: Source<Item = f32>> Iterator for TempoSource<S> {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.cursor >= self.buffer.len() && !self.refill() {
            if let Some(duration) = self.duration {
                self.control.position.store(
                    duration.as_nanos().min(u64::MAX as u128) as u64,
                    Ordering::Relaxed,
                );
            }
            return None;
        }
        let sample = self.buffer[self.cursor];
        self.cursor += 1;
        self.position += self.buffer_tempo / (self.rate as f64 * self.channels as f64);
        if self.cursor.is_multiple_of(256) || self.cursor == self.buffer.len() {
            let position = self
                .duration
                .map_or(self.position, |d| self.position.min(d.as_secs_f64()));
            self.control
                .position
                .store((position * 1_000_000_000.0) as u64, Ordering::Relaxed);
        }
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
        self.position = position.as_secs_f64();
        self.control.position.store(
            position.as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
        self.tempo = f32::from_bits(self.control.speed.load(Ordering::Relaxed)) as f64;
        self.processor.set_tempo(self.tempo);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
