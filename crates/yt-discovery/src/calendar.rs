//! Calendar arithmetic on Unix seconds at a fixed UTC offset, and the
//! half-life decay shared by every time-weighted score.

/// Seconds in one hour.
pub(crate) const SECONDS_PER_HOUR: f64 = 3_600.0;
/// Seconds in one day.
pub(crate) const SECONDS_PER_DAY: f64 = 86_400.0;
/// Hours in one day.
pub(crate) const HOURS_PER_DAY: f64 = 24.0;
/// Days in one week.
pub(crate) const DAYS_PER_WEEK: i64 = 7;
/// Largest UTC offset honored, in seconds (±18 h, the range of ISO 8601
/// offsets); larger offsets are clamped.
const MAX_UTC_OFFSET_SECONDS: i32 = 18 * 3_600;
/// Instants are clamped to ±this many seconds (about ±3 million years) so day
/// arithmetic never overflows and years fit in `i32`.
const CALENDAR_LIMIT_SECONDS: f64 = 1.0e14;
/// Weekday of 1970-01-01 counted from Monday = 0: it was a Thursday.
const EPOCH_WEEKDAY: i64 = 3;
/// Years in one cycle of the Gregorian calendar.
const YEARS_PER_ERA: i64 = 400;
/// Days in one 400-year Gregorian cycle.
const DAYS_PER_ERA: i64 = 146_097;
/// Days from 0000-03-01, the origin of the era arithmetic, to 1970-01-01.
const MARCH_ZERO_TO_EPOCH_DAYS: i64 = 719_468;
/// Days from March 1 to January 1 in the March-based years of the era
/// arithmetic.
const MARCH_TO_JANUARY_DAYS: i64 = 306;

/// Local seconds since 1970-01-01 00:00 of the instant `now` (Unix seconds)
/// at `utc_offset_seconds` east of UTC. A non-finite instant counts as the
/// epoch.
pub(crate) fn local_seconds(now: f64, utc_offset_seconds: i32) -> f64 {
    let instant = if now.is_finite() {
        now.clamp(-CALENDAR_LIMIT_SECONDS, CALENDAR_LIMIT_SECONDS)
    } else {
        0.0
    };
    instant + f64::from(utc_offset_seconds.clamp(-MAX_UTC_OFFSET_SECONDS, MAX_UTC_OFFSET_SECONDS))
}

/// Local day number (days since 1970-01-01) of `now`.
pub(crate) fn local_day(now: f64, utc_offset_seconds: i32) -> i64 {
    (local_seconds(now, utc_offset_seconds) / SECONDS_PER_DAY).floor() as i64
}

/// Local hour of day of `now`, fractional, in [0, 24] (24 only through
/// rounding, and equivalent to 0).
pub(crate) fn local_hour(now: f64, utc_offset_seconds: i32) -> f64 {
    local_seconds(now, utc_offset_seconds).rem_euclid(SECONDS_PER_DAY) / SECONDS_PER_HOUR
}

/// Day of the week of a day number, Monday = 0 through Sunday = 6.
pub(crate) fn weekday(day: i64) -> i64 {
    (day + EPOCH_WEEKDAY).rem_euclid(DAYS_PER_WEEK)
}

/// Gregorian year containing a day number, after `civil_from_days` in
/// H. Hinnant, «chrono-Compatible Low-Level Date Algorithms»; the literals
/// are that algorithm's.
pub(crate) fn year_of_day(day: i64) -> i64 {
    let shifted = day + MARCH_ZERO_TO_EPOCH_DAYS;
    let era = shifted.div_euclid(DAYS_PER_ERA);
    let day_of_era = shifted.rem_euclid(DAYS_PER_ERA);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    // Months count from March (0) to February (11); January and February
    // belong to the next civil year.
    let month = (5 * day_of_year + 2) / 153;
    year_of_era + era * YEARS_PER_ERA + i64::from(month >= 10)
}

/// Day number of January 1 of `year`, after Hinnant's `days_from_civil`.
pub(crate) fn january_first(year: i64) -> i64 {
    // January belongs to the March-based year that began the previous March.
    let march_year = year - 1;
    let era = march_year.div_euclid(YEARS_PER_ERA);
    let year_of_era = march_year.rem_euclid(YEARS_PER_ERA);
    let day_of_era =
        365 * year_of_era + year_of_era / 4 - year_of_era / 100 + MARCH_TO_JANUARY_DAYS;
    era * DAYS_PER_ERA + day_of_era - MARCH_ZERO_TO_EPOCH_DAYS
}

/// Weight of something `age_seconds` old under a half-life of
/// `half_life_days`: 1 when new, ½ after one half-life. Negative ages count
/// as new; a non-positive half-life forgets everything older than now.
pub(crate) fn half_life_weight(age_seconds: f64, half_life_days: f64) -> f64 {
    let age_days = age_seconds.max(0.0) / SECONDS_PER_DAY;
    0.5f64.powf(age_days / half_life_days.max(f64::MIN_POSITIVE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_map_to_gregorian_years_and_january_firsts() {
        assert_eq!(january_first(1970), 0);
        assert_eq!(january_first(2000), 10_957);
        assert_eq!(january_first(2026), 20_454);
        assert_eq!(january_first(1900), -25_567);
        assert_eq!(year_of_day(-1), 1969);
        assert_eq!(year_of_day(10_956), 1999);
        assert_eq!(year_of_day(10_957), 2000);
        assert_eq!(year_of_day(20_454 + 364), 2026);
        assert_eq!(year_of_day(20_454 + 365), 2027);
        for year in [1600, 1899, 1970, 2024, 2100, 2400] {
            assert_eq!(year_of_day(january_first(year)), year);
            assert_eq!(year_of_day(january_first(year) - 1), year - 1);
        }
    }

    #[test]
    fn local_time_honors_offsets_and_survives_invalid_instants() {
        // 2026-11-11 00:00 UTC is a Wednesday.
        let midnight = 1_794_355_200.0;
        assert_eq!(weekday(local_day(midnight, 0)), 2);
        assert_eq!(local_hour(midnight, -6 * 3_600), 18.0);
        assert_eq!(local_day(midnight, -6 * 3_600), local_day(midnight, 0) - 1);
        assert_eq!(local_hour(midnight, i32::MAX), 18.0);
        assert_eq!(local_day(f64::NAN, 0), 0);
        assert_eq!(local_day(f64::INFINITY, 0), 0);
        assert_eq!(
            local_day(1.0e300, 0),
            (CALENDAR_LIMIT_SECONDS / SECONDS_PER_DAY) as i64
        );
        assert!(year_of_day(local_day(1.0e300, 0)) < i64::from(i32::MAX));
        assert_eq!(half_life_weight(0.0, 30.0), 1.0);
        assert!((half_life_weight(30.0 * SECONDS_PER_DAY, 30.0) - 0.5).abs() < 1e-12);
        assert_eq!(half_life_weight(-5.0, 30.0), 1.0);
    }
}
