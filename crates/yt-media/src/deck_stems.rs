//! A deck's vocal stem during a mix: the vocals separated from a window of the
//! song, played against the rest of it with their own gains, so a mix can hold
//! one song's vocals back or let them ride over the other song's instrumental,
//! moved to its key when the keys clash ([`Stem::moved`]). The stage runs on
//! the decoded frames, before the tempo stage, so the stem lines up with the
//! song sample for sample. See `docs/systems/Dj.md`.

use crate::{
    tempo::{transpose, MAX_DECK_CHANNELS},
    MediaError, Result,
};
use rodio::{source::SeekError, Source};
use std::{
    sync::{mpsc, Arc},
    time::Duration,
};
use yt_core::TrackAnalysis;
use yt_mix::{Curve, CONTROL_FRAMES};

/// Longest stem accepted, in seconds: a mix's longest overlap and its tails.
pub const MAX_STEM_SECONDS: u32 = 120;
/// Seconds of vocals moved ahead of the frame they are heard from, so the
/// transposer has settled by then.
const MOVED_LEAD_SECONDS: f64 = 1.0;
/// Seconds over which a deck's vocals change from their own pitch to the
/// moved one: short, but long enough not to click.
const MOVED_BLEND_SECONDS: f64 = 0.02;

/// The vocals of a window of a song, interleaved at the song's decoded format.
#[derive(Debug, Clone, PartialEq)]
pub struct Stem {
    first_frame: u64,
    channels: u16,
    rate: u32,
    samples: Arc<[f32]>,
    moved: Option<Moved>,
}

/// The stem's vocals at another pitch, heard instead of its own from `from`.
#[derive(Debug, Clone, PartialEq)]
struct Moved {
    semitones: i8,
    /// Source frame from which the moved vocals sound.
    from: u64,
    /// Source frame of the first moved sample.
    first_frame: u64,
    samples: Arc<[f32]>,
}

impl Stem {
    /// The stem starting at source frame `first_frame`; fails unless the
    /// format is a deck's, the samples are whole frames of finite values and
    /// they last at most [`MAX_STEM_SECONDS`].
    pub fn new(first_frame: u64, channels: u16, rate: u32, samples: Vec<f32>) -> Result<Self> {
        let invalid = |what: &str| Err(MediaError::InvalidInput(format!("stem: {what}")));
        if !(1..=MAX_DECK_CHANNELS).contains(&channels) {
            return invalid("the channel count is out of range");
        }
        if !TrackAnalysis::SAMPLE_RATE.contains(&rate) {
            return invalid("the sample rate is out of range");
        }
        if !samples.len().is_multiple_of(usize::from(channels)) {
            return invalid("the samples are not whole frames");
        }
        let frames = samples.len() / usize::from(channels);
        if frames as u64 > u64::from(MAX_STEM_SECONDS) * u64::from(rate) {
            return invalid("it is too long");
        }
        if samples.iter().any(|sample| !sample.is_finite()) {
            return invalid("it has non-finite samples");
        }
        Ok(Self {
            first_frame,
            channels,
            rate,
            samples: samples.into(),
            moved: None,
        })
    }

    /// The same stem whose vocals sound `semitones` higher (negative: lower)
    /// from source frame `from` on, for a deck whose instrumental is silent
    /// by then; before it they sound as they are. It transposes the stem from
    /// there to its end, so call it off the interface and audio threads.
    /// Fails for a move the transposer does not make or a frame the stem
    /// does not cover.
    pub fn moved(self, semitones: i8, from: u64) -> Result<Self> {
        let covered = self.first_frame..self.first_frame + self.frames();
        if !covered.contains(&from) {
            return Err(MediaError::InvalidInput(
                "stem: it does not cover the moved vocals".into(),
            ));
        }
        let lead = (MOVED_LEAD_SECONDS * f64::from(self.rate)) as u64;
        let first_frame = from.saturating_sub(lead).max(self.first_frame);
        // Inside the stem: `from` is, and the lead never reaches before it.
        let start = (first_frame - self.first_frame) as usize * usize::from(self.channels);
        let samples = transpose(&self.samples[start..], self.channels, self.rate, semitones)?;
        Ok(Self {
            moved: Some(Moved {
                semitones,
                from,
                first_frame,
                samples: samples.into(),
            }),
            ..self
        })
    }

    /// Semitones its vocals move and the source frame they sound moved from,
    /// when [`Self::moved`] made them.
    pub fn move_of(&self) -> Option<(i8, u64)> {
        self.moved
            .as_ref()
            .map(|moved| (moved.semitones, moved.from))
    }

    /// Source frame of the stem's first sample.
    pub fn first_frame(&self) -> u64 {
        self.first_frame
    }

    /// Frames in the stem.
    pub fn frames(&self) -> u64 {
        (self.samples.len() / usize::from(self.channels)) as u64
    }

    /// Interleaved channels and sample rate.
    pub fn format(&self) -> (u16, u32) {
        (self.channels, self.rate)
    }

    /// The vocals for source frame `frame` and `channel`, if the stem covers
    /// it: the song's own, which the rest of the song is told apart by, and
    /// the ones heard (moved from [`Moved::from`] on, blended in over
    /// [`MOVED_BLEND_SECONDS`]).
    fn vocals(&self, frame: u64, channel: usize) -> Option<(f32, f32)> {
        let at = |first: u64, samples: &[f32]| {
            let offset = frame.checked_sub(first)?;
            let index = usize::try_from(offset)
                .ok()?
                .checked_mul(usize::from(self.channels))?
                .checked_add(channel)?;
            samples.get(index).copied()
        };
        let own = at(self.first_frame, &self.samples)?;
        let heard = self
            .moved
            .as_ref()
            .filter(|moved| frame >= moved.from)
            .and_then(|moved| {
                let blend = (MOVED_BLEND_SECONDS * f64::from(self.rate)).max(1.0);
                let weight = ((frame - moved.from) as f64 / blend).min(1.0) as f32;
                let other = at(moved.first_frame, &moved.samples)?;
                Some(own + (other - own) * weight)
            })
            .unwrap_or(own);
        Some((own, heard))
    }
}

/// What a deck's stage does with its stem during one mix.
pub(crate) struct DeckStem {
    stem: Stem,
    vocals: Curve,
    instrumental: Curve,
    /// Source frame heard at the mix's exit, and seconds of the mix per
    /// source frame (the inverse of the rate times the deck's speed).
    origin: u64,
    seconds_per_frame: f64,
}

impl DeckStem {
    /// `stem` with the `vocals` and `instrumental` gains over seconds since
    /// the exit, for a deck that plays source frame `origin` at the exit at
    /// `speed`.
    pub(crate) fn new(
        stem: Stem,
        vocals: Curve,
        instrumental: Curve,
        origin: u64,
        speed: f32,
    ) -> Self {
        let seconds_per_frame = 1.0 / (f64::from(stem.rate) * f64::from(speed).max(f64::EPSILON));
        Self {
            stem,
            vocals,
            instrumental,
            origin,
            seconds_per_frame,
        }
    }

    /// Both gains at source frame `frame`.
    fn gains(&self, frame: u64) -> (f32, f32) {
        let seconds = ((frame as f64 - self.origin as f64) * self.seconds_per_frame) as f32;
        (self.vocals.at(seconds), self.instrumental.at(seconds))
    }
}

/// The engine's handle on a deck's stem stage.
#[derive(Clone)]
pub(crate) struct StemControl(mpsc::Sender<Box<DeckStem>>);

impl StemControl {
    /// Use `stem` from the deck's next frame on.
    pub(crate) fn start(&self, stem: DeckStem) {
        let _ = self.0.send(Box::new(stem));
    }
}

/// Wrap `source`, whose first sample is source frame 0, in a stem stage.
pub(crate) fn stem_stage<S: Source<Item = f32>>(source: S) -> (StemStage<S>, StemControl) {
    let (sender, commands) = mpsc::channel();
    let channels = usize::from(source.channels()).clamp(1, usize::from(MAX_DECK_CHANNELS));
    let rate = source.sample_rate().max(1);
    (
        StemStage {
            source,
            channels,
            rate,
            commands,
            stem: None,
            frame: 0,
            channel: 0,
            gains: (1.0, 1.0),
            until_control: 0,
        },
        StemControl(sender),
    )
}

pub(crate) struct StemStage<S: Source<Item = f32>> {
    source: S,
    channels: usize,
    rate: u32,
    commands: mpsc::Receiver<Box<DeckStem>>,
    stem: Option<Box<DeckStem>>,
    /// Source frame of the next sample, and its channel.
    frame: u64,
    channel: usize,
    /// Vocal and instrumental gains, refreshed every `CONTROL_FRAMES`.
    gains: (f32, f32),
    until_control: usize,
}

impl<S: Source<Item = f32>> Iterator for StemStage<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let sample = self.source.next()?;
        if self.channel == 0 {
            if let Ok(stem) = self.commands.try_recv() {
                self.stem = Some(stem);
                self.until_control = 0;
            }
            if self.until_control == 0 {
                self.gains = self
                    .stem
                    .as_ref()
                    .map_or((1.0, 1.0), |stem| stem.gains(self.frame));
                self.until_control = CONTROL_FRAMES;
            }
            self.until_control -= 1;
        }
        let out = match self
            .stem
            .as_ref()
            .and_then(|stem| stem.stem.vocals(self.frame, self.channel))
        {
            Some((own, heard)) => self.gains.0 * heard + self.gains.1 * (sample - own),
            None => sample,
        };
        self.channel += 1;
        if self.channel == self.channels {
            self.channel = 0;
            self.frame += 1;
        }
        Some(out)
    }
}

impl<S: Source<Item = f32>> Source for StemStage<S> {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.source.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.source.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.source.total_duration()
    }
    /// The stage follows the seek to keep its frame count.
    fn try_seek(&mut self, position: Duration) -> std::result::Result<(), SeekError> {
        self.source.try_seek(position)?;
        self.frame = (position.as_secs_f64() * f64::from(self.rate)).round() as u64;
        self.channel = 0;
        self.until_control = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;

    const RATE: u32 = 8_000;

    fn stem(first_frame: u64, samples: Vec<f32>) -> Stem {
        Stem::new(first_frame, 1, RATE, samples).unwrap()
    }

    #[test]
    fn stems_outside_their_bounds_are_refused() {
        assert!(Stem::new(0, 0, RATE, vec![0.0]).is_err());
        assert!(Stem::new(0, 2, RATE, vec![0.0; 3]).is_err());
        assert!(Stem::new(0, 1, 4_000, vec![0.0]).is_err());
        assert!(Stem::new(0, 1, 8_000, vec![f32::NAN]).is_err());
        let too_long = vec![0.0; (u64::from(MAX_STEM_SECONDS) * 8_000 + 1) as usize];
        assert!(Stem::new(0, 1, 8_000, too_long).is_err());
        assert_eq!(stem(5, vec![0.0; 10]).frames(), 10);
    }

    #[test]
    fn without_a_stem_the_stage_passes_samples_through() {
        let input = vec![0.1, -0.2, 0.3, 0.4];
        let (stage, _) = stem_stage(SamplesBuffer::new(2, RATE, input.clone()));
        assert_eq!(stage.collect::<Vec<_>>(), input);
    }

    #[test]
    fn the_vocals_and_the_rest_take_their_own_gains_inside_the_window() {
        // A song whose every sample is 1.0, of which the vocals are 0.25.
        let (mut stage, control) = stem_stage(SamplesBuffer::new(1, RATE, vec![1.0; 400]));
        let vocals = Curve::constant(0.0);
        let instrumental = Curve::constant(1.0);
        control.start(DeckStem::new(
            stem(100, vec![0.25; 100]),
            vocals,
            instrumental,
            0,
            1.0,
        ));
        let out: Vec<f32> = (0..400).map(|_| stage.next().unwrap()).collect();
        assert_eq!(out[50], 1.0, "before the window the song is untouched");
        assert_eq!(out[150], 0.75, "the vocals are held back");
        assert_eq!(out[250], 1.0, "after the window the song is untouched");
    }

    #[test]
    fn moved_vocals_replace_the_song_s_own_from_their_frame() {
        // A song of a 220 Hz voice alone: the stem is the whole of it.
        let voice: Vec<f32> = (0..RATE * 4)
            .map(|n| (std::f32::consts::TAU * 220.0 * n as f32 / RATE as f32).sin() * 0.5)
            .collect();
        let from = u64::from(RATE) * 2;
        let moved = stem(0, voice.clone()).moved(2, from).unwrap();
        assert_eq!(moved.move_of(), Some((2, from)));
        let (mut stage, control) = stem_stage(SamplesBuffer::new(1, RATE, voice.clone()));
        // The instrumental is silent: only the vocals sound.
        control.start(DeckStem::new(
            moved,
            Curve::constant(1.0),
            Curve::constant(0.0),
            0,
            1.0,
        ));
        let out: Vec<f32> = (0..RATE * 4).map(|_| stage.next().unwrap()).collect();
        let frequency = |samples: &[f32]| {
            let crossings = samples
                .windows(2)
                .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
                .count();
            crossings as f32 * RATE as f32 / samples.len() as f32
        };
        let rate = RATE as usize;
        assert_eq!(out[..rate * 2], voice[..rate * 2], "its own pitch before");
        let after = frequency(&out[rate * 5 / 2..rate * 7 / 2]);
        assert!((after - 246.94).abs() < 3.0, "two semitones up: {after} Hz");
        let step = out[rate * 2 - 1..rate * 2 + 2]
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0, f32::max);
        assert!(step < 0.2, "the change does not click: {step}");
        assert!(stem(100, vec![0.0; 50]).moved(1, 50).is_err());
        assert!(stem(100, vec![0.0; 50]).moved(1, 150).is_err());
        assert!(stem(0, vec![0.0; 50]).moved(13, 10).is_err());
    }

    #[test]
    fn gains_follow_the_mix_clock_and_a_seek() {
        let (mut stage, control) = stem_stage(SamplesBuffer::new(1, RATE, vec![1.0; 16_000]));
        // Vocals fade out over the first second after the exit at 1 s.
        let vocals = Curve::new(vec![(0.0, 1.0), (1.0, 0.0)]).unwrap();
        control.start(DeckStem::new(
            stem(0, vec![0.5; 16_000]),
            vocals,
            Curve::constant(1.0),
            u64::from(RATE),
            1.0,
        ));
        stage.try_seek(Duration::from_millis(1_500)).unwrap();
        let out = stage.next().unwrap();
        // Half a second after the exit: the vocals at about half.
        assert!((out - 0.75).abs() < 0.02, "{out}");
    }
}
