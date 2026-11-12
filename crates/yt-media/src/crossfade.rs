//! Sample-accurate gain envelopes that blend two decks during a crossfade.
//!
//! The engine thread schedules a ramp by publishing one packed `AtomicU64`;
//! the audio thread reads it once per frame. No locks or allocations happen
//! per sample.

use crate::{deck_stems::Stem, MediaError, Result};
use rodio::{source::SeekError, Source};
use std::{
    f32::consts::FRAC_PI_2,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

/// Longest accepted crossfade.
pub const MAX_CROSSFADE: Duration = Duration::from_secs(30);

/// How two songs blend.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum FadeCurve {
    /// Constant perceived loudness: sine/cosine gains.
    #[default]
    EqualPower,
    /// Straight-line gains; the middle of the blend dips slightly.
    Linear,
    /// Smoothstep: slow start and end.
    Smooth,
}

/// A validated crossfade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crossfade {
    duration: Duration,
    curve: FadeCurve,
    /// Where the new source starts, so a blend skips its leading silence.
    start: Duration,
}

impl Crossfade {
    /// Reject zero-length and over-long blends.
    pub fn new(duration: Duration, curve: FadeCurve) -> Result<Self> {
        if duration.is_zero() || duration > MAX_CROSSFADE {
            return Err(MediaError::InvalidInput(format!(
                "El fundido debe durar entre 1 ms y {} s.",
                MAX_CROSSFADE.as_secs()
            )));
        }
        Ok(Self {
            duration,
            curve,
            start: Duration::ZERO,
        })
    }

    /// The same blend with the new source starting at `start` (where its
    /// sound starts, past any leading silence).
    pub fn starting_at(self, start: Duration) -> Self {
        Self { start, ..self }
    }

    pub fn duration(self) -> Duration {
        self.duration
    }
    pub fn curve(self) -> FadeCurve {
        self.curve
    }
    pub fn start(self) -> Duration {
        self.start
    }
}

/// How a new source replaces the playing one.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Transition {
    /// Stop the current source and start the new one at full volume.
    #[default]
    Cut,
    /// Fade the current source out while the new one fades in.
    Crossfade(Crossfade),
    /// A beat-matched mix: the new source starts on the playing one's exact
    /// exit frame and both follow the plan's lanes.
    Mix(Mix),
}

/// A validated beat-matched mix (`yt_mix::MixPlan`) for the engine, with
/// both songs' vocal stems when the plan plays them apart.
#[derive(Debug, Clone, PartialEq)]
pub struct Mix {
    plan: Arc<yt_mix::MixPlan>,
    stems: Option<Arc<[Stem; 2]>>,
    rendering: Option<Arc<Rendering>>,
}

/// The model's rendering of a fusion (`yt_mix::Fusion`): interleaved audio at
/// its format, shared with the engine thread.
#[derive(Debug, PartialEq)]
pub(crate) struct Rendering {
    pub(crate) channels: u16,
    pub(crate) rate: u32,
    pub(crate) samples: Arc<[f32]>,
}

/// How far a fusion's rendering may be from its expected length, in seconds.
pub const RENDERING_TOLERANCE_SECONDS: f64 = 0.01;

impl Mix {
    /// Wrap `plan`; fails when it breaks its bounds. A plan that uses stems
    /// needs [`Self::with_stems`], and a fusion [`Self::with_rendering`],
    /// before the engine places it.
    pub fn new(plan: yt_mix::MixPlan) -> Result<Self> {
        plan.validate()
            .map_err(|error| MediaError::InvalidInput(error.to_string()))?;
        Ok(Self {
            plan: Arc::new(plan),
            stems: None,
            rendering: None,
        })
    }

    /// Add the model's rendering of the plan's fusion: interleaved `samples`
    /// of `channels` (1 or 2) at `rate`, as long as the fusion's clip
    /// (`yt_mix::Fusion::clip_frames`) within [`RENDERING_TOLERANCE_SECONDS`].
    /// Fails for a plan without a fusion or audio that is not finite.
    pub fn with_rendering(self, samples: Vec<f32>, channels: u16, rate: u32) -> Result<Self> {
        let invalid = |what: &str| Err(MediaError::InvalidInput(format!("fusion: {what}")));
        let Some(fusion) = self.plan.fusion else {
            return invalid("the mix has no fusion");
        };
        if !(1..=2).contains(&channels) || !yt_core::TrackAnalysis::SAMPLE_RATE.contains(&rate) {
            return invalid("its format is not supported");
        }
        if !samples.len().is_multiple_of(usize::from(channels))
            || samples.iter().any(|sample| !sample.is_finite())
        {
            return invalid("its samples are not whole finite frames");
        }
        let frames = (samples.len() / usize::from(channels)) as f64;
        let expected = fusion.clip_frames(rate) as f64;
        if (frames - expected).abs() / f64::from(rate) > RENDERING_TOLERANCE_SECONDS {
            return invalid("its length is not the fusion's");
        }
        Ok(Self {
            rendering: Some(Arc::new(Rendering {
                channels,
                rate,
                samples: samples.into(),
            })),
            ..self
        })
    }

    /// Add the outgoing and incoming songs' vocal stems; fails unless the
    /// plan uses stems, each stem covers its song's frames of the plan
    /// (`MixPlan::stem_frames`) at the plan's rate, and the outgoing vocals
    /// are moved exactly as the plan moves them (`MixPlan::vocal_shift`,
    /// [`Stem::moved`]).
    pub fn with_stems(self, outgoing: Stem, incoming: Stem) -> Result<Self> {
        let frames = self
            .plan
            .stem_frames()
            .ok_or_else(|| MediaError::InvalidInput("the mix uses no stems".into()))?;
        let rates = [self.plan.outgoing_rate, self.plan.incoming_rate];
        for ((stem, range), rate) in [&outgoing, &incoming].into_iter().zip(&frames).zip(rates) {
            let covers = stem.first_frame() <= range.start
                && stem.first_frame() + stem.frames() >= range.end;
            if !covers || stem.format().1 != rate {
                return Err(MediaError::InvalidInput(
                    "a stem does not cover its song's part of the mix".into(),
                ));
            }
        }
        let planned = self
            .plan
            .vocal_shift
            .map(|shift| shift.semitones)
            .zip(self.plan.vocal_shift_frame());
        if outgoing.move_of() != planned || incoming.move_of().is_some() {
            return Err(MediaError::InvalidInput(
                "the stems' vocals are not moved as the mix plans".into(),
            ));
        }
        Ok(Self {
            stems: Some(Arc::new([outgoing, incoming])),
            ..self
        })
    }

    pub fn plan(&self) -> &yt_mix::MixPlan {
        &self.plan
    }

    /// The outgoing and incoming vocal stems, when added.
    pub(crate) fn stems(&self) -> Option<&[Stem; 2]> {
        self.stems.as_deref()
    }

    /// The fusion's rendering, when added.
    pub(crate) fn rendering(&self) -> Option<&Rendering> {
        self.rendering.as_deref()
    }

    /// A crossfade as long as the mix's overlap, for when the engine cannot
    /// place the mix on its frames.
    pub(crate) fn fallback(&self) -> Option<Crossfade> {
        let seconds = self.plan.overlap.clamp(0.001, MAX_CROSSFADE.as_secs_f32());
        let entry = self.plan.entry_frame as f64 / f64::from(self.plan.incoming_rate);
        Crossfade::new(Duration::from_secs_f32(seconds), FadeCurve::EqualPower)
            .ok()
            .map(|fade| fade.starting_at(Duration::from_secs_f64(entry)))
    }
}

/// Gain of an incoming source after `progress` (0..=1) of the blend; the
/// outgoing source uses the mirrored value, so the pair sums to unity power
/// (equal power) or unity amplitude (linear, smooth).
pub(crate) fn incoming_gain(curve: FadeCurve, progress: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    match curve {
        FadeCurve::EqualPower => (progress * FRAC_PI_2).sin(),
        FadeCurve::Linear => progress,
        FadeCurve::Smooth => progress * progress * (3.0 - 2.0 * progress),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Full = 0,
    Silent = 1,
}

const CURVE_BITS: u32 = 8;
const TARGET_BITS: u32 = 8;
const DURATION_BITS: u32 = 32;
const GENERATION_SHIFT: u32 = CURVE_BITS + TARGET_BITS + DURATION_BITS;

fn pack(generation: u16, curve: FadeCurve, target: Target, duration: Duration) -> u64 {
    let milliseconds = duration.as_millis().min(u128::from(u32::MAX)) as u64;
    (u64::from(generation) << GENERATION_SHIFT)
        | ((curve as u64) << (TARGET_BITS + DURATION_BITS))
        | ((target as u64) << DURATION_BITS)
        | milliseconds
}

fn unpack(value: u64) -> (u16, FadeCurve, Target, u64) {
    let generation = (value >> GENERATION_SHIFT) as u16;
    let curve = match (value >> (TARGET_BITS + DURATION_BITS)) & 0xff {
        1 => FadeCurve::Linear,
        2 => FadeCurve::Smooth,
        _ => FadeCurve::EqualPower,
    };
    let target = if (value >> DURATION_BITS) & 0xff == Target::Silent as u64 {
        Target::Silent
    } else {
        Target::Full
    };
    (generation, curve, target, value & u64::from(u32::MAX))
}

/// Engine-side handle of one deck's envelope.
#[derive(Clone)]
pub(crate) struct Envelope(Arc<AtomicU64>);

impl Envelope {
    fn schedule(&self, curve: FadeCurve, target: Target, duration: Duration) {
        let (generation, ..) = unpack(self.0.load(Ordering::Acquire));
        self.0.store(
            pack(generation.wrapping_add(1), curve, target, duration),
            Ordering::Release,
        );
    }
    /// Ramp to full gain.
    pub(crate) fn fade_in(&self, fade: Crossfade) {
        self.schedule(fade.curve, Target::Full, fade.duration);
    }
    /// Ramp to silence.
    pub(crate) fn fade_out(&self, fade: Crossfade) {
        self.schedule(fade.curve, Target::Silent, fade.duration);
    }
}

/// Wrap `source` in an envelope starting silent (`starts_silent`) or at full
/// gain.
pub(crate) fn envelope<S: Source<Item = f32>>(
    source: S,
    starts_silent: bool,
) -> (EnvelopeSource<S>, Envelope) {
    let control = Arc::new(AtomicU64::new(pack(
        0,
        FadeCurve::default(),
        Target::Full,
        Duration::ZERO,
    )));
    let gain = if starts_silent { 0.0 } else { 1.0 };
    (
        EnvelopeSource {
            channels: source.channels().max(1),
            sample_rate: source.sample_rate(),
            source,
            control: control.clone(),
            seen_generation: 0,
            gain,
            from: gain,
            to: gain,
            curve: FadeCurve::default(),
            ramp_frames: 0,
            ramp_position: 0,
            channel: 0,
        },
        Envelope(control),
    )
}

pub(crate) struct EnvelopeSource<S: Source<Item = f32>> {
    source: S,
    control: Arc<AtomicU64>,
    seen_generation: u16,
    gain: f32,
    from: f32,
    to: f32,
    curve: FadeCurve,
    ramp_frames: u64,
    ramp_position: u64,
    channels: u16,
    sample_rate: u32,
    channel: u16,
}

impl<S: Source<Item = f32>> EnvelopeSource<S> {
    fn begin_frame(&mut self) {
        let (generation, curve, target, milliseconds) =
            unpack(self.control.load(Ordering::Acquire));
        if generation != self.seen_generation {
            self.seen_generation = generation;
            self.from = self.gain;
            self.to = match target {
                Target::Full => 1.0,
                Target::Silent => 0.0,
            };
            self.curve = curve;
            self.ramp_frames = milliseconds * u64::from(self.sample_rate) / 1_000;
            self.ramp_position = 0;
        }
        if self.gain == self.to {
            return;
        }
        if self.ramp_position >= self.ramp_frames {
            self.gain = self.to;
            return;
        }
        self.ramp_position += 1;
        let progress = self.ramp_position as f32 / self.ramp_frames as f32;
        self.gain = if self.to >= self.from {
            self.from + (self.to - self.from) * incoming_gain(self.curve, progress)
        } else {
            self.to + (self.from - self.to) * incoming_gain(self.curve, 1.0 - progress)
        };
    }
}

impl<S: Source<Item = f32>> Iterator for EnvelopeSource<S> {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.channel == 0 {
            self.begin_frame();
        }
        let sample = self.source.next()?;
        self.channel = (self.channel + 1) % self.channels;
        Some(sample * self.gain)
    }
}

impl<S: Source<Item = f32>> Source for EnvelopeSource<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.source.current_span_len()
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn total_duration(&self) -> Option<Duration> {
        self.source.total_duration()
    }
    fn try_seek(&mut self, position: Duration) -> std::result::Result<(), SeekError> {
        self.source.try_seek(position)?;
        self.channel = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;

    const RATE: u32 = 1_000;

    /// A two-second mix at 8 kHz whose incoming vocals wait for the switch
    /// at one second, so it needs both songs' stems.
    fn stem_plan() -> yt_mix::MixPlan {
        let rate = 8_000;
        let incoming = yt_mix::Lanes {
            vocals: yt_mix::Curve::new(vec![(0.0, 0.0), (1.0, 0.0), (1.5, 1.0)]).unwrap(),
            ..yt_mix::Lanes::unchanged()
        };
        yt_mix::MixPlan {
            technique: yt_mix::Technique::Relaxed,
            outgoing_rate: rate,
            incoming_rate: rate,
            exit_frame: u64::from(rate) * 10,
            entry_frame: 0,
            speed: 1.0,
            overlap: 2.0,
            outgoing: yt_mix::Lanes::unchanged(),
            incoming,
            report: yt_mix::MixReport {
                beat_error_ms: None,
                bass_overlap_seconds: 0.0,
                loudness_step_lu: None,
            },
            fusion: None,
            vocal_shift: None,
        }
    }

    #[test]
    fn a_fusion_takes_only_a_rendering_of_its_length() {
        let fusion = yt_mix::Fusion {
            before: 4.0,
            length: 2.0,
            after: 4.0,
            seam: 0.25,
            strength: 0.5,
        };
        let plan = yt_mix::MixPlan {
            technique: yt_mix::Technique::Fusion,
            incoming: yt_mix::Lanes::unchanged(),
            fusion: Some(fusion),
            ..stem_plan()
        };
        let mix = Mix::new(plan.clone()).unwrap();
        assert!(mix.rendering().is_none());
        let frames = fusion.clip_frames(44_100) as usize;
        let with = mix
            .clone()
            .with_rendering(vec![0.1; frames * 2], 2, 44_100)
            .unwrap();
        assert_eq!(
            with.rendering().map(|rendering| rendering.channels),
            Some(2)
        );
        assert!(mix
            .clone()
            .with_rendering(vec![0.1; frames], 1, 44_100)
            .is_ok());
        assert!(mix
            .clone()
            .with_rendering(vec![0.1; (frames + 4_410) * 2], 2, 44_100)
            .is_err());
        assert!(mix
            .clone()
            .with_rendering(vec![0.1; frames * 3], 3, 44_100)
            .is_err());
        let mut broken = vec![0.1; frames * 2];
        broken[7] = f32::NAN;
        assert!(mix.with_rendering(broken, 2, 44_100).is_err());
        let plain = Mix::new(stem_plan()).unwrap();
        assert!(plain
            .with_rendering(vec![0.1; frames * 2], 2, 44_100)
            .is_err());
    }

    #[test]
    fn a_mix_takes_only_stems_that_cover_its_frames() {
        let plan = stem_plan();
        let [outgoing, incoming] = plan.stem_frames().unwrap();
        let stem = |range: &std::ops::Range<u64>, rate: u32| {
            let frames = (range.end - range.start) as usize;
            Stem::new(range.start, 2, rate, vec![0.0; frames * 2]).unwrap()
        };
        let mix = Mix::new(plan.clone()).unwrap();
        assert!(mix.stems().is_none());
        let with = mix
            .clone()
            .with_stems(stem(&outgoing, 8_000), stem(&incoming, 8_000))
            .unwrap();
        assert!(with.stems().is_some());
        let short = outgoing.start + 1..outgoing.end;
        assert!(mix
            .clone()
            .with_stems(stem(&short, 8_000), stem(&incoming, 8_000))
            .is_err());
        assert!(mix
            .with_stems(stem(&outgoing, 16_000), stem(&incoming, 8_000))
            .is_err());
        let plain = yt_mix::MixPlan {
            incoming: yt_mix::Lanes::unchanged(),
            ..plan.clone()
        };
        assert!(Mix::new(plain)
            .unwrap()
            .with_stems(stem(&outgoing, 8_000), stem(&incoming, 8_000))
            .is_err());
        // A mashup between clashing keys: the outgoing vocals ride moved from
        // the switch, where their own instrumental has left.
        let moved = yt_mix::MixPlan {
            technique: yt_mix::Technique::Mashup,
            outgoing: yt_mix::Lanes {
                instrumental: yt_mix::Curve::new(vec![(0.9, 1.0), (1.0, 0.0)]).unwrap(),
                ..yt_mix::Lanes::unchanged()
            },
            vocal_shift: Some(yt_mix::VocalShift {
                semitones: -1,
                from: 1.0,
            }),
            ..plan
        };
        let from = moved.vocal_shift_frame().unwrap();
        let [outgoing, incoming] = moved.stem_frames().unwrap();
        let mix = Mix::new(moved).unwrap();
        assert!(
            mix.clone()
                .with_stems(stem(&outgoing, 8_000), stem(&incoming, 8_000))
                .is_err(),
            "the plan moves the vocals: stems as they are do not play it"
        );
        let shifted = |semitones| stem(&outgoing, 8_000).moved(semitones, from).unwrap();
        assert!(mix
            .clone()
            .with_stems(shifted(1), stem(&incoming, 8_000))
            .is_err());
        assert!(mix.with_stems(shifted(-1), stem(&incoming, 8_000)).is_ok());
    }

    fn ones(channels: u16, frames: usize) -> SamplesBuffer {
        SamplesBuffer::new(channels, RATE, vec![1.0; frames * channels as usize])
    }

    fn fade(milliseconds: u64, curve: FadeCurve) -> Crossfade {
        Crossfade::new(Duration::from_millis(milliseconds), curve).unwrap()
    }

    #[test]
    fn curves_start_silent_end_full_and_keep_their_power_law() {
        for curve in [FadeCurve::EqualPower, FadeCurve::Linear, FadeCurve::Smooth] {
            assert_eq!(incoming_gain(curve, 0.0), 0.0);
            assert!((incoming_gain(curve, 1.0) - 1.0).abs() < 1e-6);
            assert!((incoming_gain(curve, 2.0) - 1.0).abs() < 1e-6);
        }
        for step in 0..=10 {
            let progress = step as f32 / 10.0;
            let incoming = incoming_gain(FadeCurve::EqualPower, progress);
            let outgoing = incoming_gain(FadeCurve::EqualPower, 1.0 - progress);
            assert!((incoming * incoming + outgoing * outgoing - 1.0).abs() < 1e-5);
            let linear = incoming_gain(FadeCurve::Linear, progress)
                + incoming_gain(FadeCurve::Linear, 1.0 - progress);
            assert!((linear - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn fade_in_ramps_sample_accurately_and_frames_share_one_gain() {
        let (mut source, control) = envelope(ones(2, 300), true);
        assert_eq!(source.next(), Some(0.0));
        control.fade_in(fade(100, FadeCurve::Linear));
        let samples: Vec<f32> = source.collect();
        // First a frame of silence already started, then 100 ramp frames.
        assert_eq!(samples[0], 0.0);
        for frame in samples[1..].chunks(2).take(99) {
            if frame.len() == 2 {
                assert_eq!(frame[0], frame[1]);
            }
        }
        let peak = samples.iter().copied().fold(0.0f32, f32::max);
        assert!((peak - 1.0).abs() < 1e-6);
        assert!(samples.windows(2).all(|pair| pair[1] >= pair[0] - 1e-6));
        assert!((samples[samples.len() - 1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn fade_out_reaches_silence_and_a_new_ramp_starts_from_the_current_gain() {
        let (mut source, control) = envelope(ones(1, 400), false);
        control.fade_out(fade(100, FadeCurve::EqualPower));
        let first: Vec<f32> = source.by_ref().take(50).collect();
        assert!(first[0] > 0.99);
        let midway = *first.last().unwrap();
        assert!(midway < 0.9 && midway > 0.5, "midway gain {midway}");
        control.fade_in(fade(50, FadeCurve::Linear));
        let next = source.next().unwrap();
        assert!(next >= midway && next - midway < 0.05, "{midway} -> {next}");
        let rest: Vec<f32> = source.collect();
        assert!((rest.last().unwrap() - 1.0).abs() < 1e-6);
        let (silent, control) = envelope(ones(1, 300), false);
        control.fade_out(fade(100, FadeCurve::Smooth));
        let tail: Vec<f32> = silent.skip(150).collect();
        assert!(tail.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn crossfades_are_validated_and_packing_round_trips() {
        assert!(Crossfade::new(Duration::ZERO, FadeCurve::Linear).is_err());
        assert!(
            Crossfade::new(MAX_CROSSFADE + Duration::from_millis(1), FadeCurve::Linear).is_err()
        );
        let fade = Crossfade::new(Duration::from_secs(6), FadeCurve::Smooth).unwrap();
        assert_eq!(fade.duration(), Duration::from_secs(6));
        for curve in [FadeCurve::EqualPower, FadeCurve::Linear, FadeCurve::Smooth] {
            for target in [Target::Full, Target::Silent] {
                let packed = pack(65_535, curve, target, Duration::from_millis(12_345));
                assert_eq!(unpack(packed), (65_535, curve, target, 12_345));
            }
        }
    }
}
