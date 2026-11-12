//! AI fusions: a generative model plays a beat-matched blend of two songs
//! again, as one piece of music.
//!
//! Over a fusion both songs sound together, beat-matched, the outgoing one
//! falling and the incoming one rising with equal power. A generative model
//! hears that blend with a few bars of each song around it and renders it again
//! (audio to audio, `docs/AI_DJ.md` §4.4), so the two arrangements become one.
//! The engine plays the rendering over the fusion instead of the two decks,
//! crossfading from the outgoing song over the seam before it and into the
//! incoming song over the seam after it.

use crate::{plan::ramp, Curve, PlanError};
use serde::{Deserialize, Serialize};

/// Longest fusion, in seconds (8 bars of 4 beats at 96 BPM).
pub const MAX_FUSION_SECONDS: f32 = 20.0;
/// Longest context the model hears on either side of a fusion, in seconds:
/// its cost grows with the whole canvas.
pub const MAX_FUSION_CONTEXT_SECONDS: f32 = 16.0;
/// Longest seam of a fusion, in seconds.
pub const MAX_FUSION_SEAM_SECONDS: f32 = 1.0;
/// How strongly the model renders the blend again: the noise it starts from,
/// from 0 (the blend untouched) to 1 (new music). Half keeps both songs'
/// beat, harmony and sound while making one piece of them; the beat tracker
/// follows 0.35, 0.5 and 0.65 renderings without a drift
/// (`crates/yt-worker/examples/fusion_probe.rs`, 2026-11-12).
pub const FUSION_STRENGTH: f32 = 0.5;
/// The model's starting noise for songs whose keys clash: more freedom to
/// play the two harmonies as one, while the beat tracker still follows the
/// rendering without a drift (`fusion_probe`, 2026-11-12).
pub const FUSION_CLASH_STRENGTH: f32 = 0.65;

/// Where a fusion goes, in seconds around it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Fusion {
    /// Of the outgoing song alone before the fusion, heard by the model.
    pub before: f32,
    /// The fusion itself, from the exit to the incoming song's entry.
    pub length: f32,
    /// Of the incoming song alone after the fusion, at the mix's speed,
    /// heard by the model.
    pub after: f32,
    /// Of each song crossfaded with the rendering on either side of the
    /// fusion.
    pub seam: f32,
    /// The model's starting noise, in (0, 1] ([`FUSION_STRENGTH`]).
    pub strength: f32,
}

impl Fusion {
    /// `Err` unless every length is finite and within its bound, both
    /// contexts are longer than the seam, and the strength is in (0, 1].
    pub fn validate(&self) -> Result<(), PlanError> {
        let within =
            |value: f32, low: f32, high: f32| value.is_finite() && value > low && value <= high;
        let valid = within(self.length, 0.0, MAX_FUSION_SECONDS)
            && within(self.seam, 0.0, MAX_FUSION_SEAM_SECONDS)
            && within(self.before, self.seam, MAX_FUSION_CONTEXT_SECONDS)
            && within(self.after, self.seam, MAX_FUSION_CONTEXT_SECONDS)
            && within(self.strength, 0.0, 1.0);
        if valid {
            Ok(())
        } else {
            Err(PlanError::Plan("a fusion value is out of range"))
        }
    }

    /// Frames of its rendering at `rate`: the fusion and one seam on each
    /// side.
    pub fn clip_frames(&self, rate: u32) -> u64 {
        let frames = |seconds: f32| (f64::from(seconds) * f64::from(rate)).round() as u64;
        frames(self.length) + 2 * frames(self.seam)
    }

    /// The rendering's gain in seconds since the exit: it rises over the seam
    /// before the exit while the outgoing song falls, holds over the fusion,
    /// and falls over the seam after it while the incoming song rises.
    pub fn gain(&self) -> Result<Curve, PlanError> {
        let mut points = ramp(-self.seam, 0.0, true, 1.0);
        points.extend(ramp(self.length, self.length + self.seam, false, 1.0));
        Curve::new(points)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FUSION: Fusion = Fusion {
        before: 8.0,
        length: 4.0,
        after: 8.0,
        seam: 0.5,
        strength: FUSION_STRENGTH,
    };

    #[test]
    fn a_fusion_is_bounded_and_needs_context_around_its_seams() {
        assert!(FUSION.validate().is_ok());
        for broken in [
            Fusion {
                length: 0.0,
                ..FUSION
            },
            Fusion {
                length: MAX_FUSION_SECONDS + 0.1,
                ..FUSION
            },
            Fusion {
                before: 0.4,
                ..FUSION
            },
            Fusion {
                after: MAX_FUSION_CONTEXT_SECONDS + 1.0,
                ..FUSION
            },
            Fusion {
                seam: f32::NAN,
                ..FUSION
            },
            Fusion {
                seam: 1.5,
                ..FUSION
            },
            Fusion {
                strength: 0.0,
                ..FUSION
            },
            Fusion {
                strength: 1.2,
                ..FUSION
            },
        ] {
            assert!(broken.validate().is_err(), "{broken:?}");
        }
    }

    #[test]
    fn the_rendering_crossfades_with_both_songs_over_its_seams() {
        assert_eq!(FUSION.clip_frames(44_100), 4 * 44_100 + 2 * 22_050);
        let gain = FUSION.gain().unwrap();
        assert_eq!(gain.at(-1.0), 0.0);
        assert!(gain.at(-0.25) > 0.5);
        assert_eq!(gain.at(0.0), 1.0);
        assert_eq!(gain.at(2.0), 1.0);
        assert!(gain.at(4.25) > 0.5);
        assert_eq!(gain.at(5.0), 0.0);
    }
}
