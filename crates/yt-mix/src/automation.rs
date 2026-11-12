//! Automation: what one deck does over the seconds of a mix.

use crate::{
    dsp::{FILTER_OCTAVES, MIN_EQ_DB},
    PlanError,
};
use std::ops::RangeInclusive;

/// A piecewise-linear value over seconds since the mix starts (negative
/// before it). Before its first point it holds the first value, after its
/// last point the last one.
#[derive(Debug, Clone, PartialEq)]
pub struct Curve {
    points: Vec<(f32, f32)>,
}

impl Curve {
    /// Most points in one curve.
    pub const MAX_POINTS: usize = 16;

    /// A curve that always holds `value`.
    pub fn constant(value: f32) -> Self {
        Self {
            points: vec![(0.0, value)],
        }
    }

    /// A curve through `points` (seconds, value): 1 to [`Self::MAX_POINTS`]
    /// finite points in time order.
    pub fn new(points: Vec<(f32, f32)>) -> Result<Self, PlanError> {
        if points.is_empty() || points.len() > Self::MAX_POINTS {
            return Err(PlanError::Curve("a curve has 1 to 16 points"));
        }
        if points
            .iter()
            .any(|(time, value)| !time.is_finite() || !value.is_finite())
        {
            return Err(PlanError::Curve("every point is finite"));
        }
        if points.windows(2).any(|pair| pair[1].0 < pair[0].0) {
            return Err(PlanError::Curve("points are in time order"));
        }
        Ok(Self { points })
    }

    /// The value at `seconds`.
    pub fn at(&self, seconds: f32) -> f32 {
        let (Some(&(start, first)), Some(&(_, last))) = (self.points.first(), self.points.last())
        else {
            return 0.0;
        };
        if seconds <= start {
            return first;
        }
        for pair in self.points.windows(2) {
            let ((from_time, from), (to_time, to)) = (pair[0], pair[1]);
            if seconds <= to_time {
                if to_time <= from_time {
                    return to;
                }
                return from + (to - from) * (seconds - from_time) / (to_time - from_time);
            }
        }
        last
    }

    /// Seconds of the first and the last point.
    pub fn span(&self) -> (f32, f32) {
        let first = self.points.first().map_or(0.0, |point| point.0);
        let last = self.points.last().map_or(0.0, |point| point.0);
        (first, last)
    }

    fn within(&self, range: &RangeInclusive<f32>) -> bool {
        self.points.iter().all(|(_, value)| range.contains(value))
    }
}

/// The DSP settings of one deck at one instant: a linear gain (0–1), EQ cuts
/// in dB ([`MIN_EQ_DB`]–0), filter cutoffs in octaves above
/// [`crate::FILTER_FLOOR_HZ`] (0 leaves the high-pass open, [`FILTER_OCTAVES`]
/// the low-pass) and the reverb send (0–1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeckParams {
    pub gain: f32,
    pub low_db: f32,
    pub mid_db: f32,
    pub high_db: f32,
    pub highpass: f32,
    pub lowpass: f32,
    pub reverb_send: f32,
}

/// The effects a deck's mix uses; their lines are allocated when its
/// processor is built.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Effects {
    pub reverb: bool,
}

impl Effects {
    pub const NONE: Self = Self { reverb: false };

    /// Whether the deck keeps sounding after its gain reaches zero.
    pub fn rings(&self) -> bool {
        self.reverb
    }
}

impl DeckParams {
    /// A deck that plays unchanged.
    pub const UNCHANGED: Self = Self {
        gain: 1.0,
        low_db: 0.0,
        mid_db: 0.0,
        high_db: 0.0,
        highpass: 0.0,
        lowpass: FILTER_OCTAVES,
        reverb_send: 0.0,
    };
}

/// What one deck does during a mix, lane by lane, in the units of
/// [`DeckParams`].
#[derive(Debug, Clone, PartialEq)]
pub struct Lanes {
    pub gain: Curve,
    pub low_db: Curve,
    pub mid_db: Curve,
    pub high_db: Curve,
    pub highpass: Curve,
    pub lowpass: Curve,
    pub reverb_send: Curve,
    /// Gains of the deck's separated vocals and of the rest of it; both 1
    /// unless the mix uses stems (`uses_stems`), which the engine then plays
    /// before the tempo stage, outside [`DeckParams`].
    pub vocals: Curve,
    pub instrumental: Curve,
    pub effects: Effects,
}

impl Lanes {
    /// A deck that plays unchanged.
    pub fn unchanged() -> Self {
        let params = DeckParams::UNCHANGED;
        Self {
            gain: Curve::constant(params.gain),
            low_db: Curve::constant(params.low_db),
            mid_db: Curve::constant(params.mid_db),
            high_db: Curve::constant(params.high_db),
            highpass: Curve::constant(params.highpass),
            lowpass: Curve::constant(params.lowpass),
            reverb_send: Curve::constant(params.reverb_send),
            vocals: Curve::constant(1.0),
            instrumental: Curve::constant(1.0),
            effects: Effects::NONE,
        }
    }

    /// Whether the deck's vocals and the rest of it take their own gains, so
    /// the mix needs the deck's vocal stem.
    pub fn uses_stems(&self) -> bool {
        let unchanged = Curve::constant(1.0);
        self.vocals != unchanged || self.instrumental != unchanged
    }

    /// Everything the deck does at `seconds`.
    pub fn at(&self, seconds: f32) -> DeckParams {
        DeckParams {
            gain: self.gain.at(seconds),
            low_db: self.low_db.at(seconds),
            mid_db: self.mid_db.at(seconds),
            high_db: self.high_db.at(seconds),
            highpass: self.highpass.at(seconds),
            lowpass: self.lowpass.at(seconds),
            reverb_send: self.reverb_send.at(seconds),
        }
    }

    /// Seconds of the earliest and the latest change of any lane.
    pub fn span(&self) -> (f32, f32) {
        self.curves().iter().map(|curve| curve.span()).fold(
            (f32::INFINITY, f32::NEG_INFINITY),
            |(start, end), (from, to)| (start.min(from), end.max(to)),
        )
    }

    /// `Err` when a lane leaves the range of its unit.
    pub fn validate(&self) -> Result<(), PlanError> {
        let unit = 0.0..=1.0;
        let eq = MIN_EQ_DB..=0.0;
        let filter = 0.0..=FILTER_OCTAVES;
        let valid = self.gain.within(&unit)
            && self.reverb_send.within(&unit)
            && self.vocals.within(&unit)
            && self.instrumental.within(&unit)
            && self.low_db.within(&eq)
            && self.mid_db.within(&eq)
            && self.high_db.within(&eq)
            && self.highpass.within(&filter)
            && self.lowpass.within(&filter);
        if valid {
            Ok(())
        } else {
            Err(PlanError::Plan("a lane leaves the range of its unit"))
        }
    }

    fn curves(&self) -> [&Curve; 9] {
        [
            &self.gain,
            &self.low_db,
            &self.mid_db,
            &self.high_db,
            &self.highpass,
            &self.lowpass,
            &self.reverb_send,
            &self.vocals,
            &self.instrumental,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_interpolate_and_hold_their_ends() {
        let curve = Curve::new(vec![(0.0, 0.0), (2.0, 1.0), (2.0, 0.5), (4.0, 0.5)]).unwrap();
        assert_eq!(curve.at(-1.0), 0.0);
        assert_eq!(curve.at(1.0), 0.5);
        assert_eq!(
            curve.at(2.0),
            1.0,
            "a step takes its first value at its time"
        );
        assert_eq!(curve.at(3.0), 0.5);
        assert_eq!(curve.at(10.0), 0.5);
        assert_eq!(curve.span(), (0.0, 4.0));
    }

    #[test]
    fn invalid_curves_are_refused() {
        assert!(Curve::new(Vec::new()).is_err());
        assert!(Curve::new(vec![(1.0, 0.0), (0.0, 1.0)]).is_err());
        assert!(Curve::new(vec![(0.0, f32::NAN)]).is_err());
        assert!(Curve::new(vec![(0.0, 0.0); Curve::MAX_POINTS + 1]).is_err());
    }

    #[test]
    fn unchanged_lanes_are_the_identity_and_ranges_are_checked() {
        let lanes = Lanes::unchanged();
        assert_eq!(lanes.at(5.0), DeckParams::UNCHANGED);
        assert!(lanes.validate().is_ok());
        let boosted = Lanes {
            low_db: Curve::constant(3.0),
            ..Lanes::unchanged()
        };
        assert!(boosted.validate().is_err(), "the EQ only cuts");
    }
}
