//! The engine's side of a beat-matched mix (`docs/AI_DJ.md` §5.7). Every deck
//! carries a [`MixStage`]: idle, it passes samples through; during a mix it
//! runs the plan's lanes through a `yt_mix::DeckProcessor`. The incoming
//! deck's stage also gates its start: it stays silent, without reading its
//! source, until the outgoing deck's clock reaches the exit frame, so the two
//! songs meet on the same sample whatever the timing of the commands.

use crate::tempo::{TempoControl, MAX_DECK_CHANNELS};
use rodio::{source::SeekError, Source};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};
use yt_mix::{DeckParams, DeckProcessor, Lanes, CONTROL_FRAMES};

/// What one deck does during one mix. Built on the engine thread, where the
/// processor allocates its echo line, then moved to the audio thread.
pub(crate) struct DeckMix {
    lanes: Arc<Lanes>,
    processor: DeckProcessor,
    clock: Clock,
    /// Seconds after the exit when the lanes hold their last values.
    end: f32,
    done: bool,
}

enum Clock {
    /// Seconds since the exit, from the deck's own source clock.
    Outgoing {
        own: TempoControl,
        exit_frame: u64,
        rate: f64,
    },
    /// Silent until `gate`, the outgoing deck's clock, reaches `gate_frame`
    /// (or the engine sets `force`); then `at` plus the seconds since that
    /// moment, counted in the frames this deck has played.
    Incoming {
        gate: TempoControl,
        gate_frame: u64,
        at: f64,
        gate_rate: f64,
        force: Arc<AtomicBool>,
        started: Option<f64>,
        frames: u64,
        rate: f64,
    },
}

impl DeckMix {
    /// The outgoing deck's part, timed by its own clock.
    pub(crate) fn outgoing(
        lanes: Arc<Lanes>,
        own: TempoControl,
        exit_frame: u64,
        format: (u16, u32),
        end: f32,
    ) -> Self {
        Self {
            processor: DeckProcessor::new(format.0, format.1, lanes.effects),
            lanes,
            clock: Clock::Outgoing {
                own,
                exit_frame,
                rate: f64::from(format.1),
            },
            end,
            done: false,
        }
    }

    /// The part of a deck that starts during the mix: silent until the
    /// outgoing deck's clock `gate` (at `gate_rate`) reaches `gate_frame`,
    /// where its lanes are `at` seconds since the exit (0 for the incoming
    /// song of most mixes).
    #[allow(clippy::too_many_arguments)] // Every value places the gate; a struct would only rename them.
    pub(crate) fn incoming(
        lanes: Arc<Lanes>,
        gate: TempoControl,
        gate_frame: u64,
        at: f32,
        gate_rate: u32,
        force: Arc<AtomicBool>,
        format: (u16, u32),
        end: f32,
    ) -> Self {
        Self {
            processor: DeckProcessor::new(format.0, format.1, lanes.effects),
            lanes,
            clock: Clock::Incoming {
                gate,
                gate_frame,
                at: f64::from(at),
                gate_rate: f64::from(gate_rate),
                force,
                started: None,
                frames: 0,
                rate: f64::from(format.1),
            },
            end,
            done: false,
        }
    }

    /// Seconds since the exit for the next frame; `None` while the incoming
    /// deck waits for it.
    fn seconds(&mut self) -> Option<f32> {
        match &mut self.clock {
            Clock::Outgoing {
                own,
                exit_frame,
                rate,
            } => Some(((own.frame() as f64 - *exit_frame as f64) / *rate) as f32),
            Clock::Incoming {
                gate,
                gate_frame,
                at,
                gate_rate,
                force,
                started,
                frames,
                rate,
            } => {
                let start = match started {
                    Some(start) => *start,
                    None => {
                        let now = gate.frame();
                        if now < *gate_frame && !force.load(Ordering::Relaxed) {
                            return None;
                        }
                        let start = *at + now.saturating_sub(*gate_frame) as f64 / *gate_rate;
                        *started = Some(start);
                        start
                    }
                };
                Some((start + *frames as f64 / *rate) as f32)
            }
        }
    }

    fn played_frame(&mut self) {
        if let Clock::Incoming {
            frames,
            started: Some(_),
            ..
        } = &mut self.clock
        {
            *frames += 1;
        }
    }
}

/// The engine's handle on a playing deck's stage.
#[derive(Clone)]
pub(crate) struct MixControl(mpsc::Sender<Box<DeckMix>>);

impl MixControl {
    /// Start `mix` on the deck at its next frame.
    pub(crate) fn start(&self, mix: DeckMix) {
        let _ = self.0.send(Box::new(mix));
    }
}

/// Wrap `source` in a stage, already running `mix` when given (the incoming
/// deck of a mix).
pub(crate) fn mix_stage<S: Source<Item = f32>>(
    source: S,
    mix: Option<DeckMix>,
) -> (MixStage<S>, MixControl) {
    let (sender, commands) = mpsc::channel();
    let channels = usize::from(source.channels()).clamp(1, usize::from(MAX_DECK_CHANNELS));
    (
        MixStage {
            source,
            channels,
            commands,
            mix: mix.map(Box::new),
            frame: [0.0; MAX_DECK_CHANNELS as usize],
            filled: 0,
            cursor: 0,
            channel: 0,
            until_control: 0,
        },
        MixControl(sender),
    )
}

pub(crate) struct MixStage<S: Source<Item = f32>> {
    source: S,
    channels: usize,
    commands: mpsc::Receiver<Box<DeckMix>>,
    mix: Option<Box<DeckMix>>,
    frame: [f32; MAX_DECK_CHANNELS as usize],
    filled: usize,
    cursor: usize,
    /// Position inside the current frame while samples pass through.
    channel: usize,
    until_control: usize,
}

impl<S: Source<Item = f32>> MixStage<S> {
    /// The next frame through the mix, returning its first sample.
    fn mixed_frame(&mut self) -> Option<f32> {
        let channels = self.channels;
        let mix = self.mix.as_mut()?;
        let Some(seconds) = mix.seconds() else {
            self.frame[..channels].fill(0.0);
            self.filled = channels;
            self.cursor = 1;
            return Some(0.0);
        };
        if self.until_control == 0 {
            let params = mix.lanes.at(seconds);
            mix.processor.set(&params);
            mix.done = seconds > mix.end && params == DeckParams::UNCHANGED;
            self.until_control = CONTROL_FRAMES;
        }
        self.until_control -= 1;
        for index in 0..channels {
            match self.source.next() {
                Some(sample) => self.frame[index] = sample,
                None if index == 0 => return None,
                None => self.frame[index] = 0.0,
            }
        }
        mix.processor.process(&mut self.frame[..channels]);
        mix.played_frame();
        self.filled = channels;
        self.cursor = 1;
        Some(self.frame[0])
    }
}

impl<S: Source<Item = f32>> Iterator for MixStage<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.cursor < self.filled {
            let sample = self.frame[self.cursor];
            self.cursor += 1;
            return Some(sample);
        }
        if self.channel == 0 {
            if let Ok(mix) = self.commands.try_recv() {
                self.mix = Some(mix);
                self.until_control = 0;
            }
            if self.mix.as_ref().is_some_and(|mix| !mix.done) {
                return self.mixed_frame();
            }
        }
        let sample = self.source.next()?;
        self.channel = (self.channel + 1) % self.channels;
        Some(sample)
    }
}

impl<S: Source<Item = f32>> Source for MixStage<S> {
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
    /// A seek ends the mix: the deck plays unchanged, and an incoming deck
    /// still waiting starts at once.
    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        self.source.try_seek(position)?;
        self.filled = 0;
        self.cursor = 0;
        self.channel = 0;
        if let Some(mix) = self.mix.as_mut() {
            mix.done = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tempo::TempoSource;
    use rodio::buffer::SamplesBuffer;
    use yt_mix::Curve;

    const RATE: u32 = 1_000;

    fn ramp(frames: usize) -> Vec<f32> {
        (0..frames).map(|frame| 1.0 + frame as f32).collect()
    }

    #[test]
    fn an_idle_stage_passes_samples_through() {
        let input = vec![0.1, 0.2, 0.3, 0.4];
        let (stage, _) = mix_stage(SamplesBuffer::new(2, RATE, input.clone()), None);
        assert_eq!(stage.collect::<Vec<_>>(), input);
    }

    #[test]
    fn the_incoming_deck_starts_on_the_exact_exit_frame() {
        let (mut outgoing, clock) =
            TempoSource::new(SamplesBuffer::new(1, RATE, ramp(100)), 1.0).unwrap();
        let force = Arc::new(AtomicBool::new(false));
        let mix = DeckMix::incoming(
            Arc::new(Lanes::unchanged()),
            clock,
            40,
            0.0,
            RATE,
            force,
            (1, RATE),
            0.0,
        );
        let (mut incoming, _) = mix_stage(SamplesBuffer::new(1, RATE, vec![0.5; 100]), Some(mix));
        let mut heard = Vec::new();
        for _ in 0..60 {
            let _ = outgoing.next();
            heard.push(incoming.next().unwrap());
        }
        let first = heard.iter().position(|sample| *sample != 0.0).unwrap();
        assert_eq!(first, 39, "silent until the outgoing deck plays frame 40");
        assert!(heard[first..].iter().all(|sample| *sample == 0.5));
    }

    #[test]
    fn a_later_gate_starts_its_lanes_at_their_own_time() {
        let lanes = Arc::new(Lanes {
            gain: Curve::new(vec![(0.4, 0.0), (0.5, 1.0)]).unwrap(),
            ..Lanes::unchanged()
        });
        // The gate opens at frame 40; at the exit (`at` 0) the lanes are still
        // silent, half a second after it they are full.
        for (at, expected) in [(0.0, 0.0), (0.5, 0.5)] {
            let (mut outgoing, clock) =
                TempoSource::new(SamplesBuffer::new(1, RATE, ramp(100)), 1.0).unwrap();
            let mix = DeckMix::incoming(
                lanes.clone(),
                clock,
                40,
                at,
                RATE,
                Arc::new(AtomicBool::new(false)),
                (1, RATE),
                1.0,
            );
            let (mut incoming, _) =
                mix_stage(SamplesBuffer::new(1, RATE, vec![0.5; 100]), Some(mix));
            let heard: Vec<f32> = (0..60)
                .map(|_| {
                    let _ = outgoing.next();
                    incoming.next().unwrap()
                })
                .collect();
            assert!(heard[..39].iter().all(|sample| *sample == 0.0));
            assert!(
                (heard[45] - expected).abs() < 1e-6,
                "at {at}: {}",
                heard[45]
            );
        }
    }

    #[test]
    fn a_forced_gate_opens_without_the_outgoing_clock() {
        let (_, clock) = TempoSource::new(SamplesBuffer::new(1, RATE, ramp(10)), 1.0).unwrap();
        let force = Arc::new(AtomicBool::new(false));
        let mix = DeckMix::incoming(
            Arc::new(Lanes::unchanged()),
            clock,
            1_000,
            0.0,
            RATE,
            force.clone(),
            (1, RATE),
            0.0,
        );
        let (mut incoming, _) = mix_stage(SamplesBuffer::new(1, RATE, vec![0.5; 10]), Some(mix));
        assert_eq!(incoming.next(), Some(0.0));
        force.store(true, Ordering::Relaxed);
        assert_eq!(incoming.next(), Some(0.5));
    }

    #[test]
    fn the_outgoing_lanes_follow_its_own_clock() {
        let (source, clock) =
            TempoSource::new(SamplesBuffer::new(1, RATE, vec![1.0; 200]), 1.0).unwrap();
        let lanes = Lanes {
            gain: Curve::new(vec![(0.0, 1.0), (0.0, 0.0)]).unwrap(),
            ..Lanes::unchanged()
        };
        let (stage, control) = mix_stage(source, None);
        control.start(DeckMix::outgoing(
            Arc::new(lanes),
            clock,
            100,
            (1, RATE),
            1.0,
        ));
        let output: Vec<f32> = stage.collect();
        assert!(output[..100].iter().all(|sample| *sample == 1.0));
        assert!(
            output[100 + CONTROL_FRAMES..]
                .iter()
                .all(|sample| *sample == 0.0),
            "silent from the first control update after the exit"
        );
    }

    #[test]
    fn a_seek_ends_the_mix_and_opens_the_gate() {
        let (_, clock) = TempoSource::new(SamplesBuffer::new(1, RATE, ramp(10)), 1.0).unwrap();
        let mix = DeckMix::incoming(
            Arc::new(Lanes::unchanged()),
            clock,
            1_000,
            0.0,
            RATE,
            Arc::new(AtomicBool::new(false)),
            (1, RATE),
            0.0,
        );
        let (mut incoming, _) = mix_stage(SamplesBuffer::new(1, RATE, ramp(10)), Some(mix));
        assert_eq!(incoming.next(), Some(0.0));
        incoming.try_seek(Duration::from_millis(2)).unwrap();
        assert_eq!(incoming.next(), Some(3.0));
    }
}
