//! The DSP of one deck during a mix: a three-band DJ EQ (cuts only), a
//! high-pass and a low-pass filter that sweep, a reverb and the gain. [`DeckProcessor::new`] allocates the effects' lines
//! (call it off the audio thread); [`DeckProcessor::set`] and
//! [`DeckProcessor::process`] never allocate, lock or block.

use crate::{DeckParams, Effects};
use std::f32::consts::{FRAC_1_SQRT_2, PI};

/// Channels a deck may have; further channels pass through unprocessed.
pub const MAX_CHANNELS: usize = 8;
/// Corner of the EQ's low shelf, in Hz: the bass a DJ swaps.
pub const LOW_SHELF_HZ: f32 = 200.0;
/// Centre of the EQ's mid band, in Hz.
pub const MID_PEAK_HZ: f32 = 1_000.0;
/// Corner of the EQ's high shelf, in Hz.
pub const HIGH_SHELF_HZ: f32 = 4_000.0;
/// Deepest EQ cut, in dB: a kill.
pub const MIN_EQ_DB: f32 = -30.0;
/// Cutoff of a filter at 0 octaves, in Hz.
pub const FILTER_FLOOR_HZ: f32 = 20.0;
/// Octaves from [`FILTER_FLOOR_HZ`] to the top of hearing (20.5 kHz).
pub const FILTER_OCTAVES: f32 = 10.0;
/// Frames between two updates of a deck's parameters by its caller.
pub const CONTROL_FRAMES: usize = 64;
/// Reverb comb and all-pass lengths at 44.1 kHz (Freeverb's tuning), and the
/// extra samples of the right channel's all-passes that spread it in stereo.
const COMB_TUNING: [usize; 4] = [1_116, 1_188, 1_277, 1_356];
const ALLPASS_TUNING: [usize; 2] = [556, 441];
const STEREO_SPREAD: usize = 23;
const TUNING_RATE: f32 = 44_100.0;
/// Reverb feedback (room size), damping of its highs, all-pass feedback, and
/// the level of the wet signal: a tail of about two and a half seconds.
const REVERB_FEEDBACK: f32 = 0.84;
const REVERB_DAMPING: f32 = 0.2;
const ALLPASS_FEEDBACK: f32 = 0.5;
const REVERB_WET: f32 = 0.3;
/// Changes smaller than this (in dB or octaves) keep the coefficients.
const SETTLED: f32 = 0.01;
/// Highest cutoff relative to the sample rate, below Nyquist with margin.
const MAX_CUTOFF_RATIO: f32 = 0.45;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Coefficients {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

/// Biquads from the RBJ Audio EQ Cookbook.
impl Coefficients {
    const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn normalized(b: [f32; 3], a: [f32; 3]) -> Self {
        Self {
            b0: b[0] / a[0],
            b1: b[1] / a[0],
            b2: b[2] / a[0],
            a1: a[1] / a[0],
            a2: a[2] / a[0],
        }
    }

    fn angle(rate: f32, hz: f32) -> (f32, f32) {
        let hz = hz.clamp(1.0, rate * MAX_CUTOFF_RATIO);
        let omega = 2.0 * PI * hz / rate;
        (omega.cos(), omega.sin())
    }

    fn low_shelf(rate: f32, hz: f32, db: f32) -> Self {
        let a = 10_f32.powf(db / 40.0);
        let (cos, sin) = Self::angle(rate, hz);
        let alpha = sin / 2.0 * 2_f32.sqrt();
        let root = 2.0 * a.sqrt() * alpha;
        Self::normalized(
            [
                a * ((a + 1.0) - (a - 1.0) * cos + root),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - root),
            ],
            [
                (a + 1.0) + (a - 1.0) * cos + root,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - root,
            ],
        )
    }

    fn high_shelf(rate: f32, hz: f32, db: f32) -> Self {
        let a = 10_f32.powf(db / 40.0);
        let (cos, sin) = Self::angle(rate, hz);
        let alpha = sin / 2.0 * 2_f32.sqrt();
        let root = 2.0 * a.sqrt() * alpha;
        Self::normalized(
            [
                a * ((a + 1.0) + (a - 1.0) * cos + root),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - root),
            ],
            [
                (a + 1.0) - (a - 1.0) * cos + root,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - root,
            ],
        )
    }

    fn peaking(rate: f32, hz: f32, db: f32) -> Self {
        let a = 10_f32.powf(db / 40.0);
        let (cos, sin) = Self::angle(rate, hz);
        let alpha = sin / (2.0 * FRAC_1_SQRT_2);
        Self::normalized(
            [1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a],
            [1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a],
        )
    }

    fn highpass(rate: f32, hz: f32) -> Self {
        let (cos, sin) = Self::angle(rate, hz);
        let alpha = sin / (2.0 * FRAC_1_SQRT_2);
        Self::normalized(
            [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0],
            [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
        )
    }

    fn lowpass(rate: f32, hz: f32) -> Self {
        let (cos, sin) = Self::angle(rate, hz);
        let alpha = sin / (2.0 * FRAC_1_SQRT_2);
        Self::normalized(
            [(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0],
            [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
        )
    }
}

/// The low shelf, mid peak, high shelf, high-pass and low-pass, in order.
const STAGES: usize = 5;

/// A delay line with feedback through a one-pole low-pass.
struct Comb {
    line: Vec<f32>,
    index: usize,
    filtered: f32,
}

impl Comb {
    fn process(&mut self, input: f32) -> f32 {
        let output = self.line[self.index];
        self.filtered = output * (1.0 - REVERB_DAMPING) + self.filtered * REVERB_DAMPING;
        self.line[self.index] = input + self.filtered * REVERB_FEEDBACK;
        self.index = (self.index + 1) % self.line.len();
        output
    }
}

struct Allpass {
    line: Vec<f32>,
    index: usize,
}

impl Allpass {
    fn process(&mut self, input: f32) -> f32 {
        let buffered = self.line[self.index];
        self.line[self.index] = input + buffered * ALLPASS_FEEDBACK;
        self.index = (self.index + 1) % self.line.len();
        buffered - input
    }
}

/// A small Schroeder reverb: parallel combs, then all-passes per side.
struct Reverb {
    combs: Vec<Comb>,
    left: Vec<Allpass>,
    right: Vec<Allpass>,
}

impl Reverb {
    fn new(rate: f32) -> Self {
        let length =
            |samples: usize| ((samples as f32 * rate / TUNING_RATE).round() as usize).max(1);
        let allpasses = |spread: usize| {
            ALLPASS_TUNING
                .iter()
                .map(|samples| Allpass {
                    line: vec![0.0; length(samples + spread)],
                    index: 0,
                })
                .collect()
        };
        Self {
            combs: COMB_TUNING
                .iter()
                .map(|samples| Comb {
                    line: vec![0.0; length(*samples)],
                    index: 0,
                    filtered: 0.0,
                })
                .collect(),
            left: allpasses(0),
            right: allpasses(STEREO_SPREAD),
        }
    }

    /// The left and right wet signal for one mono input sample.
    fn process(&mut self, input: f32) -> (f32, f32) {
        let mut sum = 0.0;
        for comb in &mut self.combs {
            sum += comb.process(input);
        }
        let mut left = sum;
        for allpass in &mut self.left {
            left = allpass.process(left);
        }
        let mut right = sum;
        for allpass in &mut self.right {
            right = allpass.process(right);
        }
        (left * REVERB_WET, right * REVERB_WET)
    }
}

/// The DSP one deck runs during a mix.
pub struct DeckProcessor {
    rate: f32,
    channels: usize,
    params: DeckParams,
    coefficients: [Coefficients; STAGES],
    active: [bool; STAGES],
    state: [[[f32; 2]; STAGES]; MAX_CHANNELS],
    reverb: Option<Reverb>,
}

impl DeckProcessor {
    /// A processor for `channels` interleaved channels at `sample_rate`,
    /// with the lines of `effects` (a reverb). It starts at
    /// [`DeckParams::UNCHANGED`].
    pub fn new(channels: u16, sample_rate: u32, effects: Effects) -> Self {
        let channels = usize::from(channels).clamp(1, MAX_CHANNELS);
        let rate = sample_rate.max(1) as f32;
        Self {
            rate,
            channels,
            params: DeckParams::UNCHANGED,
            coefficients: [Coefficients::IDENTITY; STAGES],
            active: [false; STAGES],
            state: [[[0.0; 2]; STAGES]; MAX_CHANNELS],
            reverb: effects.reverb.then(|| Reverb::new(rate)),
        }
    }

    /// Apply `params`, recomputing only the stages that changed.
    pub fn set(&mut self, params: &DeckParams) {
        let previous = self.params;
        let changed = |now: f32, before: f32| (now - before).abs() >= SETTLED;
        let stages = [
            (
                params.low_db < -SETTLED,
                changed(params.low_db, previous.low_db),
            ),
            (
                params.mid_db < -SETTLED,
                changed(params.mid_db, previous.mid_db),
            ),
            (
                params.high_db < -SETTLED,
                changed(params.high_db, previous.high_db),
            ),
            (
                params.highpass > SETTLED,
                changed(params.highpass, previous.highpass),
            ),
            (
                params.lowpass < FILTER_OCTAVES - SETTLED,
                changed(params.lowpass, previous.lowpass),
            ),
        ];
        for (stage, (active, moved)) in stages.into_iter().enumerate() {
            if active && !self.active[stage] {
                for channel in &mut self.state {
                    channel[stage] = [0.0; 2];
                }
            }
            self.active[stage] = active;
            if active && (moved || self.coefficients[stage] == Coefficients::IDENTITY) {
                self.coefficients[stage] = self.coefficients_for(stage, params);
            }
        }
        self.params = DeckParams {
            gain: params.gain.clamp(0.0, 1.0),
            reverb_send: params.reverb_send.clamp(0.0, 1.0),
            ..*params
        };
    }

    /// Process one interleaved frame in place.
    pub fn process(&mut self, frame: &mut [f32]) {
        let channels = self.channels.min(frame.len());
        let mut filtered = [0.0_f32; MAX_CHANNELS];
        for (channel, sample) in frame.iter().take(channels).enumerate() {
            let mut value = *sample;
            for stage in 0..STAGES {
                if self.active[stage] {
                    let c = self.coefficients[stage];
                    let [z1, z2] = &mut self.state[channel][stage];
                    let out = c.b0 * value + *z1;
                    *z1 = c.b1 * value - c.a1 * out + *z2;
                    *z2 = c.b2 * value - c.a2 * out;
                    value = out;
                }
            }
            filtered[channel] = value;
        }
        let wet = match &mut self.reverb {
            Some(reverb) => {
                let mono = filtered[..channels].iter().sum::<f32>() / channels as f32;
                reverb.process(mono * self.params.reverb_send)
            }
            None => (0.0, 0.0),
        };
        for (channel, sample) in frame.iter_mut().take(channels).enumerate() {
            let mut output = filtered[channel] * self.params.gain;
            output += match (channels, channel % 2) {
                (1, _) => (wet.0 + wet.1) / 2.0,
                (_, 0) => wet.0,
                _ => wet.1,
            };
            *sample = output;
        }
    }

    fn coefficients_for(&self, stage: usize, params: &DeckParams) -> Coefficients {
        let octaves = |value: f32| FILTER_FLOOR_HZ * 2_f32.powf(value.clamp(0.0, FILTER_OCTAVES));
        match stage {
            0 => Coefficients::low_shelf(self.rate, LOW_SHELF_HZ, params.low_db.max(MIN_EQ_DB)),
            1 => Coefficients::peaking(self.rate, MID_PEAK_HZ, params.mid_db.max(MIN_EQ_DB)),
            2 => Coefficients::high_shelf(self.rate, HIGH_SHELF_HZ, params.high_db.max(MIN_EQ_DB)),
            3 => Coefficients::highpass(self.rate, octaves(params.highpass)),
            _ => Coefficients::lowpass(self.rate, octaves(params.lowpass)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// RMS level in dB of a sine at `hz` after `params`, once the filters settle.
    fn level(hz: f32, params: DeckParams) -> f32 {
        let mut processor = DeckProcessor::new(1, RATE, Effects::NONE);
        processor.set(&params);
        let mut energy = 0.0_f64;
        let frames = RATE as usize;
        for index in 0..frames {
            let mut frame = [(2.0 * PI * hz * index as f32 / RATE as f32).sin()];
            processor.process(&mut frame);
            if index >= frames / 2 {
                energy += f64::from(frame[0] * frame[0]);
            }
        }
        let rms = (energy / (frames / 2) as f64).sqrt() * 2_f64.sqrt();
        20.0 * rms.log10() as f32
    }

    #[test]
    fn unchanged_parameters_leave_the_audio_untouched() {
        let mut processor = DeckProcessor::new(2, RATE, Effects::NONE);
        processor.set(&DeckParams::UNCHANGED);
        let mut frame = [0.25, -0.5];
        processor.process(&mut frame);
        assert_eq!(frame, [0.25, -0.5]);
    }

    #[test]
    fn a_low_kill_removes_the_bass_and_keeps_the_rest() {
        let kill = DeckParams {
            low_db: MIN_EQ_DB,
            ..DeckParams::UNCHANGED
        };
        assert!(level(50.0, kill) < -25.0, "{}", level(50.0, kill));
        assert!(level(3_000.0, kill).abs() < 1.0, "{}", level(3_000.0, kill));
    }

    #[test]
    fn filters_sweep_where_their_octaves_say() {
        let highpass = DeckParams {
            highpass: 7.0,
            ..DeckParams::UNCHANGED
        };
        assert!(level(100.0, highpass) < -25.0);
        assert!(level(10_000.0, highpass).abs() < 1.0);
        let lowpass = DeckParams {
            lowpass: 4.0,
            ..DeckParams::UNCHANGED
        };
        assert!(level(5_000.0, lowpass) < -20.0);
        assert!(level(60.0, lowpass).abs() < 1.0);
    }

    #[test]
    fn the_reverb_rings_after_its_input_stops_and_dies_away() {
        let mut processor = DeckProcessor::new(2, RATE, Effects { reverb: true });
        processor.set(&DeckParams {
            gain: 0.0,
            reverb_send: 1.0,
            ..DeckParams::UNCHANGED
        });
        let energy = |from: usize, to: usize, processor: &mut DeckProcessor| {
            let mut sum = 0.0_f32;
            for index in from..to {
                let input = if index < 4_800 { 0.5 } else { 0.0 };
                let mut frame = [input, input];
                processor.process(&mut frame);
                sum += frame[0] * frame[0] + frame[1] * frame[1];
            }
            sum
        };
        let _ = energy(0, 4_800, &mut processor);
        let early = energy(4_800, 14_400, &mut processor);
        let _ = energy(14_400, 4 * RATE as usize, &mut processor);
        let late = energy(4 * RATE as usize, 5 * RATE as usize, &mut processor);
        assert!(early > 0.1, "the tail rings: {early}");
        assert!(
            late < early / 1_000.0,
            "the tail dies away: {late} after {early}"
        );
    }
}
