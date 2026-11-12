//! Listening by time of day: the songs the user plays in the morning,
//! afternoon, evening or night.

use crate::{
    calendar::{local_hour, HOURS_PER_DAY},
    ranking::SKIP_LIMIT,
    Profile,
};
use std::{collections::HashMap, f64::consts::TAU};
use yt_core::ListenSignal;

/// Hours in one day, as an integer hour count.
const DAY_HOURS: u32 = HOURS_PER_DAY as u32;
/// First local hour of the morning.
const MORNING_START_HOUR: u32 = 5;
/// First local hour of the afternoon.
const AFTERNOON_START_HOUR: u32 = 12;
/// First local hour of the evening.
const EVENING_START_HOUR: u32 = 18;
/// First local hour of the night, which lasts until the morning.
const NIGHT_START_HOUR: u32 = 22;
/// Concentration κ of the von Mises kernel around a part's center; the
/// kernel halves about 3.3 hours away.
const DAY_PART_CONCENTRATION: f64 = 2.0;
/// Listen score added to every song's total before its share in a part is
/// measured, so a single listen is weak evidence.
const DAY_PART_PRIOR_SCORE: f64 = 1.0;
/// Most terms of the series for the Bessel function I₀; the terms fall
/// factorially, so the series converges far sooner.
const BESSEL_MAX_TERMS: u32 = 64;

/// A part of the local day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DayPart {
    /// 05:00 to 11:59.
    Morning,
    /// 12:00 to 17:59.
    Afternoon,
    /// 18:00 to 21:59.
    Evening,
    /// 22:00 to 04:59.
    Night,
}

impl DayPart {
    /// Every part, in the order of the day.
    pub const ALL: [DayPart; 4] = [
        DayPart::Morning,
        DayPart::Afternoon,
        DayPart::Evening,
        DayPart::Night,
    ];

    /// The part holding local `hour`, taken modulo 24.
    pub fn from_hour(hour: u32) -> Self {
        match hour % DAY_HOURS {
            hour if hour < MORNING_START_HOUR => DayPart::Night,
            hour if hour < AFTERNOON_START_HOUR => DayPart::Morning,
            hour if hour < EVENING_START_HOUR => DayPart::Afternoon,
            hour if hour < NIGHT_START_HOUR => DayPart::Evening,
            _ => DayPart::Night,
        }
    }

    /// Stable identifier for keys and storage.
    pub fn key(self) -> &'static str {
        match self {
            DayPart::Morning => "morning",
            DayPart::Afternoon => "afternoon",
            DayPart::Evening => "evening",
            DayPart::Night => "night",
        }
    }

    /// The part whose [`key`](Self::key) is `key`.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|part| part.key() == key)
    }

    fn start_hour(self) -> u32 {
        match self {
            DayPart::Morning => MORNING_START_HOUR,
            DayPart::Afternoon => AFTERNOON_START_HOUR,
            DayPart::Evening => EVENING_START_HOUR,
            DayPart::Night => NIGHT_START_HOUR,
        }
    }

    /// Local hour at the middle of the part; past 24 for the night.
    fn center_hour(self) -> f64 {
        let next = match self {
            DayPart::Morning => AFTERNOON_START_HOUR,
            DayPart::Afternoon => EVENING_START_HOUR,
            DayPart::Evening => NIGHT_START_HOUR,
            DayPart::Night => MORNING_START_HOUR,
        };
        let span = (next + DAY_HOURS - self.start_hour()) % DAY_HOURS;
        f64::from(self.start_hour()) + f64::from(span) / 2.0
    }
}

/// The part of the day `now` (Unix seconds) falls in, in local time at
/// `utc_offset_seconds` east of UTC (clamped to ±18 h).
pub fn day_part(now: f64, utc_offset_seconds: i32) -> DayPart {
    DayPart::from_hour(local_hour(now, utc_offset_seconds).floor() as u32)
}

/// Up to `limit` liked media whose listening concentrates in `part`, best
/// first.
///
/// Every listen in `signals` (local time at `utc_offset_seconds`) adds its
/// score (`profile.config().listen_score`, negative for skips) weighted by a
/// von Mises kernel exp(κ (cos Δθ − 1)) of its angular distance to the part's
/// center (κ = 2). A song's share in the part is that sum over its total
/// absolute score plus a one-listen prior. Only songs whose share beats a
/// listener without time habits, e^(−κ) I₀(κ) ≈ 0.31, take part, and only
/// liked ones (positive affinity in `profile`) skipped fewer than twice. They
/// rank by share × tanh(affinity); ties break by id.
pub fn day_part_mix(
    signals: &[ListenSignal],
    profile: &Profile,
    part: DayPart,
    utc_offset_seconds: i32,
    limit: usize,
) -> Vec<String> {
    let center = part.center_hour();
    let mut sums: HashMap<&str, (f64, f64)> = HashMap::new();
    for signal in signals
        .iter()
        .filter(|signal| signal.started_at.is_finite())
    {
        let score = profile.config().listen_score(signal);
        let hours = local_hour(signal.started_at, utc_offset_seconds) - center;
        let (near, total) = sums.entry(signal.media_id.as_str()).or_default();
        *near += score * kernel(hours);
        *total += score.abs();
    }
    let baseline = uniform_kernel_mean();
    let mut ranked: Vec<(f64, &str)> = sums
        .into_iter()
        .filter_map(|(media_id, (near, total))| {
            let known = profile.media_affinity(media_id)?;
            let share = near / (total + DAY_PART_PRIOR_SCORE);
            (known.score > 0.0 && known.skips < SKIP_LIMIT && share > baseline)
                .then(|| (share * known.score.tanh(), media_id))
        })
        .collect();
    ranked.sort_by(|left, right| right.0.total_cmp(&left.0).then_with(|| left.1.cmp(right.1)));
    ranked
        .into_iter()
        .take(limit)
        .map(|(_, media_id)| media_id.to_owned())
        .collect()
}

/// Von Mises kernel of a distance in hours on the 24-hour circle: 1 at the
/// center, e^(−2κ) opposite it.
fn kernel(hours: f64) -> f64 {
    (DAY_PART_CONCENTRATION * ((TAU * hours / HOURS_PER_DAY).cos() - 1.0)).exp()
}

/// Mean of [`kernel`] over a uniform day: e^(−κ) I₀(κ).
fn uniform_kernel_mean() -> f64 {
    (-DAY_PART_CONCENTRATION).exp() * bessel_i0(DAY_PART_CONCENTRATION)
}

/// Modified Bessel function of the first kind, order 0, by its power series
/// Σ ((x/2)^k / k!)².
fn bessel_i0(x: f64) -> f64 {
    let quarter_square = x * x / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..BESSEL_MAX_TERMS {
        term *= quarter_square / f64::from(k * k);
        sum += term;
        if term <= sum * f64::EPSILON {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        test_support::{completed, skipped, DAY, HOUR, NOW},
        AffinityConfig,
    };

    const MEXICO_CITY: i32 = -6 * 3_600;

    /// `NOW` is 18:00 local in Mexico City; `hour` is a local hour there.
    fn local(days_ago: f64, hour: f64) -> f64 {
        NOW - days_ago * DAY + (hour - 18.0) * HOUR
    }

    #[test]
    fn hours_map_to_parts_and_keys_round_trip() {
        use DayPart::{Afternoon, Evening, Morning, Night};
        let parts = [0, 4, 5, 11, 12, 17, 18, 21, 22, 23, 24, 29].map(DayPart::from_hour);
        assert_eq!(
            parts,
            [
                Night, Night, Morning, Morning, Afternoon, Afternoon, Evening, Evening, Night,
                Night, Night, Morning
            ]
        );
        assert_eq!(day_part(NOW, 0), Night);
        assert_eq!(day_part(NOW, MEXICO_CITY), Evening);
        assert_eq!(Night.center_hour(), 25.5);
        assert_eq!(Morning.center_hour(), 8.5);
        for part in DayPart::ALL {
            assert_eq!(DayPart::from_key(part.key()), Some(part));
        }
        assert!((uniform_kernel_mean() - 0.3085).abs() < 1e-3);
    }

    #[test]
    fn night_mixes_prefer_songs_heard_at_night() {
        let mut signals = Vec::new();
        for (day, hour) in [(1.0, 23.0), (2.0, 0.5), (3.0, 1.0), (4.0, 2.0), (5.0, 23.5)] {
            signals.push(completed("night", "Owl", local(day, hour)));
            signals.push(completed("afternoon", "Lark", local(day, 14.0)));
            // Five listens spread evenly over the day: no time habit.
            signals.push(completed("anytime", "Clock", local(day, 4.8 * day)));
        }
        signals.push(completed("once", "One", local(1.0, 1.0)));
        signals.push(completed("disliked", "Bad", local(1.0, 0.0)));
        signals.push(skipped("disliked", "Bad", local(2.0, 0.0)));
        signals.push(skipped("disliked", "Bad", local(3.0, 0.0)));
        let profile = Profile::build(&signals, &[], NOW, &AffinityConfig::default());
        let night = day_part_mix(&signals, &profile, DayPart::Night, MEXICO_CITY, 10);
        assert_eq!(night, ["night", "once"]);
        assert_eq!(
            day_part_mix(&signals, &profile, DayPart::Afternoon, MEXICO_CITY, 10),
            ["afternoon"]
        );
        assert_eq!(
            day_part_mix(&signals, &profile, DayPart::Night, MEXICO_CITY, 1),
            ["night"]
        );
        // The same instants read in UTC put the night songs in the morning.
        let morning = day_part_mix(&signals, &profile, DayPart::Morning, 0, 10);
        assert!(morning.contains(&"night".to_owned()));
    }
}
