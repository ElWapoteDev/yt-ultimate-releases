//! Seeded pseudo-random numbers, stable hashing and week-stable seeds.
//!
//! Engines never use unseeded randomness: every draw comes from a
//! [`SeededRng`] whose seed the caller chooses, so equal inputs give equal
//! output.

use crate::calendar::{january_first, local_day, weekday, year_of_day, DAYS_PER_WEEK};

/// SplitMix64 increment: the golden ratio in 64-bit fixed point.
const SPLITMIX_GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;
/// First SplitMix64 finalizer multiplier.
const SPLITMIX_MULTIPLIER_A: u64 = 0xbf58_476d_1ce4_e5b9;
/// Second SplitMix64 finalizer multiplier.
const SPLITMIX_MULTIPLIER_B: u64 = 0x94d0_49bb_1331_11eb;
/// Bits of a draw kept for a uniform double: the 53 of an `f64` mantissa.
const UNIT_BITS: u32 = 53;
/// Scale from a 53-bit integer to [0, 1).
const UNIT_SCALE: f64 = 1.0 / (1u64 << UNIT_BITS) as f64;
/// Squeeze constant of the Marsaglia–Tsang gamma sampler.
const MARSAGLIA_TSANG_SQUEEZE: f64 = 0.0331;
/// FNV-1a 64-bit offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
/// Weekday, Monday = 0, of the Thursday that names an ISO 8601 week.
const ISO_WEEK_THURSDAY: i64 = 3;

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(SPLITMIX_GAMMA);
    let mut mixed = *state;
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(SPLITMIX_MULTIPLIER_A);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(SPLITMIX_MULTIPLIER_B);
    mixed ^ (mixed >> 31)
}

/// A seeded pseudo-random generator: xoshiro256** (Blackman & Vigna 2021)
/// with its state filled by SplitMix64 (Steele, Lea & Flood 2014).
///
/// Equal seeds give equal sequences on every platform. Fast and well
/// distributed, but not for cryptography.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeededRng {
    state: [u64; 4],
}

impl SeededRng {
    /// Smallest shape [`gamma`](Self::gamma) and [`beta`](Self::beta)
    /// accept; smaller shapes put essentially all mass at 0.
    pub const MIN_SHAPE: f64 = 1.0e-6;

    /// A generator whose whole sequence is determined by `seed`.
    pub fn new(seed: u64) -> Self {
        let mut state = seed;
        Self {
            state: [
                splitmix64(&mut state),
                splitmix64(&mut state),
                splitmix64(&mut state),
                splitmix64(&mut state),
            ],
        }
    }

    /// The next 64 uniformly distributed bits.
    pub fn next_u64(&mut self) -> u64 {
        let [first, second, third, fourth] = &mut self.state;
        let result = second.wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let shifted = *second << 17;
        *third ^= *first;
        *fourth ^= *second;
        *second ^= *third;
        *first ^= *fourth;
        *third ^= shifted;
        *fourth = fourth.rotate_left(45);
        result
    }

    /// A uniform double in [0, 1) with 53 random bits.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> (u64::BITS - UNIT_BITS)) as f64 * UNIT_SCALE
    }

    /// A standard normal draw (mean 0, variance 1) by the Box–Muller
    /// transform.
    pub fn normal(&mut self) -> f64 {
        // 1 − u lies in (0, 1], so the logarithm is finite.
        let radius = (-2.0 * (1.0 - self.next_f64()).ln()).sqrt();
        radius * (std::f64::consts::TAU * self.next_f64()).cos()
    }

    /// A Gamma(`shape`, 1) draw by Marsaglia & Tsang (2000); its mean is
    /// `shape`. Returns NaN, like `f64::ln` outside its domain, when `shape`
    /// is not finite or is below [`MIN_SHAPE`](Self::MIN_SHAPE).
    pub fn gamma(&mut self, shape: f64) -> f64 {
        if !valid_shape(shape) {
            return f64::NAN;
        }
        self.ln_gamma_draw(shape).exp()
    }

    /// A Beta(`a`, `b`) draw in [0, 1], as X / (X + Y) of two gamma draws;
    /// its mean is a / (a + b). Returns NaN when either shape is not finite
    /// or is below [`MIN_SHAPE`](Self::MIN_SHAPE).
    pub fn beta(&mut self, a: f64, b: f64) -> f64 {
        if !valid_shape(a) || !valid_shape(b) {
            return f64::NAN;
        }
        let x = self.ln_gamma_draw(a);
        let y = self.ln_gamma_draw(b);
        // X / (X + Y) = 1 / (1 + e^(ln Y − ln X)); logs keep tiny draws exact.
        1.0 / (1.0 + (y - x).exp())
    }

    /// Natural logarithm of a Gamma(`shape`, 1) draw for a valid shape.
    fn ln_gamma_draw(&mut self, shape: f64) -> f64 {
        if shape < 1.0 {
            // Gamma(a) = Gamma(a + 1) · U^(1/a) (Marsaglia & Tsang 2000);
            // kept in logs so small shapes do not underflow to 0.
            let boost = (1.0 - self.next_f64()).ln() / shape;
            return self.marsaglia_tsang(shape + 1.0).ln() + boost;
        }
        self.marsaglia_tsang(shape).ln()
    }

    /// Squeeze-and-reject gamma sampler for `shape` ≥ 1; accepts more than
    /// 95 % of proposals, so the loop ends after about one round.
    fn marsaglia_tsang(&mut self, shape: f64) -> f64 {
        let d = shape - 1.0 / 3.0;
        let c = 1.0 / (9.0 * d).sqrt();
        loop {
            let x = self.normal();
            let v = 1.0 + c * x;
            if v <= 0.0 {
                continue;
            }
            let v = v * v * v;
            let u = 1.0 - self.next_f64();
            if u < 1.0 - MARSAGLIA_TSANG_SQUEEZE * x.powi(4)
                || u.ln() < 0.5 * x * x + d * (1.0 - v + v.ln())
            {
                return d * v;
            }
        }
    }
}

fn valid_shape(shape: f64) -> bool {
    shape.is_finite() && shape >= SeededRng::MIN_SHAPE
}

/// A well-mixed seed from two values; different pairs give unrelated seeds.
pub(crate) fn mix_seed(seed: u64, value: u64) -> u64 {
    let mut state = seed;
    let mut state = splitmix64(&mut state) ^ value;
    splitmix64(&mut state)
}

/// FNV-1a over `salt` and `text`; a fixed, documented hash keeps tie-breaks
/// stable across builds and platforms.
pub(crate) fn stable_hash(salt: u64, text: &str) -> u64 {
    salt.to_le_bytes()
        .iter()
        .chain(text.as_bytes())
        .fold(FNV_OFFSET, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
        })
}

/// A seed shared by every instant of one week, from Monday 00:00 to Sunday
/// 24:00 local time, mixed with `salt`; consecutive weeks get unrelated
/// seeds.
///
/// `now` is Unix seconds and `utc_offset_seconds` the local offset east of
/// UTC, clamped to ±18 h. A non-finite `now` counts as the epoch.
pub fn week_seed(now: f64, utc_offset_seconds: i32, salt: u64) -> u64 {
    let day = local_day(now, utc_offset_seconds);
    let monday = day - weekday(day);
    // The two's-complement bits identify the Monday; only equality matters.
    mix_seed(salt, monday as u64)
}

/// ISO 8601 week-numbering year and week (1–53) of `now` in local time.
///
/// Weeks start on Monday and week 1 is the week holding the year's first
/// Thursday, so the last days of December can belong to week 1 of the next
/// year and the first days of January to week 52 or 53 of the previous one.
/// Same inputs as [`week_seed`].
pub fn iso_week(now: f64, utc_offset_seconds: i32) -> (i32, u32) {
    let day = local_day(now, utc_offset_seconds);
    let thursday = day - weekday(day) + ISO_WEEK_THURSDAY;
    let year = year_of_day(thursday);
    let week = (thursday - january_first(year)) / DAYS_PER_WEEK + 1;
    // The calendar clamp keeps years within ±3.2 million and weeks in 1..=53.
    (year as i32, week as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DRAWS: usize = 20_000;
    /// 2026-11-11 00:00 UTC, a Wednesday of ISO week 46.
    const WEDNESDAY: f64 = 1_794_355_200.0;
    const HOUR: f64 = 3_600.0;
    const DAY: f64 = 86_400.0;
    const MEXICO_CITY: i32 = -6 * 3_600;

    fn mean(draws: impl Iterator<Item = f64>) -> f64 {
        let values: Vec<f64> = draws.collect();
        values.iter().sum::<f64>() / values.len() as f64
    }

    #[test]
    fn equal_seeds_repeat_and_different_seeds_diverge() {
        let mut first = SeededRng::new(7);
        let mut second = SeededRng::new(7);
        let mut other = SeededRng::new(8);
        let sequence: Vec<u64> = (0..16).map(|_| first.next_u64()).collect();
        assert_eq!(
            sequence,
            (0..16).map(|_| second.next_u64()).collect::<Vec<_>>()
        );
        assert_ne!(
            sequence,
            (0..16).map(|_| other.next_u64()).collect::<Vec<_>>()
        );
        let mut rng = SeededRng::new(1);
        assert!((0..DRAWS)
            .map(|_| rng.next_f64())
            .all(|value| (0.0..1.0).contains(&value)));
    }

    #[test]
    fn distributions_have_their_documented_means() {
        let mut rng = SeededRng::new(42);
        assert!((mean((0..DRAWS).map(|_| rng.next_f64())) - 0.5).abs() < 0.01);
        assert!(mean((0..DRAWS).map(|_| rng.normal())).abs() < 0.03);
        let variance = mean((0..DRAWS).map(|_| rng.normal().powi(2)));
        assert!((variance - 1.0).abs() < 0.05);
        assert!((mean((0..DRAWS).map(|_| rng.gamma(3.0))) - 3.0).abs() < 0.08);
        assert!((mean((0..DRAWS).map(|_| rng.gamma(0.4))) - 0.4).abs() < 0.03);
        let beta = mean((0..DRAWS).map(|_| rng.beta(2.0, 5.0)));
        assert!((beta - 2.0 / 7.0).abs() < 0.01, "Beta(2, 5) mean {beta}");
        let small = mean((0..DRAWS).map(|_| rng.beta(0.05, 0.05)));
        assert!((small - 0.5).abs() < 0.03, "Beta(0.05, 0.05) mean {small}");
        assert!((0..DRAWS)
            .map(|_| rng.beta(0.01, 3.0))
            .all(|value| (0.0..=1.0).contains(&value)));
    }

    #[test]
    fn invalid_shapes_return_nan_instead_of_looping() {
        let mut rng = SeededRng::new(3);
        for shape in [0.0, -1.0, f64::NAN, f64::INFINITY, 1.0e-9] {
            assert!(rng.gamma(shape).is_nan(), "gamma({shape})");
            assert!(rng.beta(shape, 1.0).is_nan(), "beta({shape}, 1)");
            assert!(rng.beta(1.0, shape).is_nan(), "beta(1, {shape})");
        }
    }

    #[test]
    fn iso_weeks_follow_the_first_thursday_rule() {
        assert_eq!(iso_week(WEDNESDAY, 0), (2026, 46));
        // 2021-01-03 (Sunday) closes 2020-W53; 2021-01-04 opens 2021-W01.
        assert_eq!(iso_week(1_609_675_200.0, 0), (2020, 53));
        assert_eq!(iso_week(1_609_718_400.0, 0), (2021, 1));
        // 2024-12-30 (Monday) already belongs to 2025-W01.
        assert_eq!(iso_week(1_735_516_800.0, 0), (2025, 1));
        // 2021-01-04 05:30 UTC is still Sunday 2021-01-03 in Mexico City.
        assert_eq!(iso_week(1_609_738_200.0, 0), (2021, 1));
        assert_eq!(iso_week(1_609_738_200.0, MEXICO_CITY), (2020, 53));
        assert_eq!(iso_week(f64::NAN, 0), (1970, 1));
    }

    #[test]
    fn week_seeds_hold_for_a_local_week_and_change_at_local_monday() {
        let salt = 99;
        let seed = week_seed(WEDNESDAY, MEXICO_CITY, salt);
        // Sunday 23:30 in Mexico City is Monday 05:30 UTC.
        let sunday_night = WEDNESDAY + 5.0 * DAY + 5.5 * HOUR;
        assert_eq!(week_seed(sunday_night, MEXICO_CITY, salt), seed);
        assert_ne!(
            week_seed(sunday_night, 0, salt),
            week_seed(WEDNESDAY, 0, salt)
        );
        let monday_midnight = WEDNESDAY + 5.0 * DAY + 6.0 * HOUR;
        assert_ne!(week_seed(monday_midnight, MEXICO_CITY, salt), seed);
        assert_eq!(
            week_seed(monday_midnight, MEXICO_CITY, salt),
            week_seed(monday_midnight + 6.0 * DAY, MEXICO_CITY, salt)
        );
        assert_ne!(week_seed(WEDNESDAY, MEXICO_CITY, salt + 1), seed);
        assert_eq!(stable_hash(0, "x"), stable_hash(0, "x"));
        assert_ne!(stable_hash(0, "x"), stable_hash(1, "x"));
    }
}
