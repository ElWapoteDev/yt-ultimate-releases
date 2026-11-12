//! Karaoke timing: the line being sung, how its text fills, and when a
//! running fill no longer agrees with the media clock.

use yt_core::Lyrics;

/// How lines without word timing fill while they are sung.
#[derive(Debug, Clone, PartialEq)]
pub struct KaraokeConfig {
    /// Slowest pace a line is assumed to be sung at, in milliseconds per
    /// character. When the next line starts later, the rest of the gap is an
    /// instrumental break and the line stays filled through it.
    pub max_ms_per_char: u32,
    /// Shortest fill, in milliseconds, so a short line never just flashes.
    pub min_fill_ms: u32,
}

impl Default for KaraokeConfig {
    fn default() -> Self {
        Self {
            max_ms_per_char: 180,
            min_fill_ms: 400,
        }
    }
}

/// A stretch of a line that fills from `start_ms` to `end_ms` (media time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub start_ms: u32,
    pub end_ms: u32,
}

impl Segment {
    /// The fill animation of this segment as seen at lyrics time `now_ms`
    /// while media plays at `rate`: its delay (negative once it started) and
    /// its length, both in wall-clock milliseconds. A rate outside 0.25–4×
    /// is clamped to that range.
    pub fn schedule(&self, now_ms: f64, rate: f64) -> (f64, f64) {
        let rate = if rate.is_finite() {
            rate.clamp(0.25, 4.0)
        } else {
            1.0
        };
        let delay = (f64::from(self.start_ms) - now_ms) / rate;
        let length = f64::from(self.end_ms.saturating_sub(self.start_ms)) / rate;
        (delay, length)
    }
}

/// The line sung at `position_ms` of lyrics time: the latest start at or
/// before it. Lines sharing that start (a duet, or a translation under its
/// original) are sung together; the first of them is returned. `None`
/// before the first line and for plain lyrics.
pub fn active_line(lyrics: &Lyrics, position_ms: f64) -> Option<usize> {
    if !lyrics.is_synced() || !position_ms.is_finite() {
        return None;
    }
    let lines = lyrics.lines();
    let latest = lines
        .partition_point(|line| f64::from(line.start_ms) <= position_ms)
        .checked_sub(1)?;
    let start = lines[latest].start_ms;
    Some(lines.partition_point(|line| line.start_ms < start))
}

/// How line `index` fills: one segment per timed word, otherwise the whole
/// line spread over the time it is likely sung (the gap to the next line,
/// capped by `KaraokeConfig::max_ms_per_char`). Empty for plain lyrics, an
/// empty line or an index out of range.
pub fn line_segments(lyrics: &Lyrics, index: usize, config: &KaraokeConfig) -> Vec<Segment> {
    let lines = lyrics.lines();
    let Some(line) = lines
        .get(index)
        .filter(|line| lyrics.is_synced() && !line.text.is_empty())
    else {
        return Vec::new();
    };
    let next_start = lines[index + 1..]
        .iter()
        .map(|next| next.start_ms)
        .find(|start| *start > line.start_ms);
    let paced_end = |start: u32, text: &str| {
        let chars = u32::try_from(text.chars().count()).unwrap_or(u32::MAX);
        let paced = start.saturating_add(
            chars
                .saturating_mul(config.max_ms_per_char)
                .max(config.min_fill_ms),
        );
        next_start.map_or(paced, |next| paced.min(next))
    };
    if line.words.is_empty() {
        return vec![Segment {
            text: line.text.clone(),
            start_ms: line.start_ms,
            end_ms: paced_end(line.start_ms, &line.text),
        }];
    }
    line.words
        .iter()
        .enumerate()
        .map(|(position, word)| {
            let end_ms = match line.words.get(position + 1) {
                Some(following) => following.start_ms,
                None => line
                    .end_ms
                    .unwrap_or_else(|| paced_end(word.start_ms, &word.text)),
            };
            Segment {
                text: word.text.clone(),
                start_ms: word.start_ms,
                end_ms: end_ms.max(word.start_ms),
            }
        })
        .collect()
}

/// Follows a fill scheduled from one position sample and reports when later
/// samples no longer match where the fill is: after a seek, a rate change or
/// drift between the media clock and the animation clock.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncClock {
    anchor_ms: f64,
    rate: f64,
    advanced_ms: f64,
}

impl SyncClock {
    /// Largest disagreement, in milliseconds, before a fill is rescheduled.
    pub const TOLERANCE_MS: f64 = 350.0;

    /// A clock anchored at the sample the fill was scheduled from.
    pub fn new(position_ms: f64, rate: f64) -> Self {
        Self {
            anchor_ms: position_ms,
            rate,
            advanced_ms: 0.0,
        }
    }

    /// Account for `wall_ms` of wall time since the previous sample, during
    /// which media advanced if `running`, then compare the new sample.
    /// Returns `true`, and re-anchors at the sample, when the fill must be
    /// rescheduled.
    pub fn observe(&mut self, position_ms: f64, rate: f64, wall_ms: f64, running: bool) -> bool {
        if running && wall_ms.is_finite() {
            self.advanced_ms += wall_ms.max(0.0) * self.rate;
        }
        let expected = self.anchor_ms + self.advanced_ms;
        let reschedule = !position_ms.is_finite()
            || (rate - self.rate).abs() > f64::EPSILON
            || (position_ms - expected).abs() > Self::TOLERANCE_MS;
        if reschedule {
            *self = Self::new(position_ms, rate);
        }
        reschedule
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lyrics(text: &str) -> Lyrics {
        Lyrics::parse(text).unwrap()
    }

    #[test]
    fn the_active_line_is_the_first_of_the_latest_start() {
        let song = lyrics("[00:01.00]One\n[00:03.00]Two\n[00:03.00]Twice\n[00:08.00]Three");
        assert_eq!(active_line(&song, 0.0), None);
        assert_eq!(active_line(&song, 1_000.0), Some(0));
        assert_eq!(active_line(&song, 2_999.0), Some(0));
        assert_eq!(active_line(&song, 3_000.0), Some(1));
        assert_eq!(active_line(&song, 99_000.0), Some(3));
        assert_eq!(active_line(&song, f64::NAN), None);
        assert_eq!(active_line(&lyrics("Plain"), 5_000.0), None);
    }

    #[test]
    fn lines_fill_over_the_sung_time_and_hold_through_breaks() {
        let config = KaraokeConfig::default();
        let song = lyrics(
            "[00:01.00]Hello there\n[00:02.00]Short gap\n[01:00.00]After a break\n[01:01.00]",
        );
        let fills = |index| {
            line_segments(&song, index, &config)
                .iter()
                .map(|segment| (segment.start_ms, segment.end_ms))
                .collect::<Vec<_>>()
        };
        assert_eq!(fills(0), [(1_000, 2_000)]);
        assert_eq!(fills(1), [(2_000, 2_000 + 9 * 180)]);
        assert_eq!(fills(2), [(60_000, 61_000)]);
        assert!(fills(3).is_empty());
        assert!(fills(9).is_empty());
    }

    #[test]
    fn timed_words_fill_one_by_one_up_to_the_marked_end() {
        let config = KaraokeConfig::default();
        let song = lyrics("[00:10.00]<00:10.00>Hel<00:10.40>lo <00:11.00>world<00:12.50>\n[00:20.00]<00:20.00>Next");
        let segments = line_segments(&song, 0, &config);
        assert_eq!(
            segments
                .iter()
                .map(|segment| (segment.text.as_str(), segment.start_ms, segment.end_ms))
                .collect::<Vec<_>>(),
            [
                ("Hel", 10_000, 10_400),
                ("lo ", 10_400, 11_000),
                ("world", 11_000, 12_500)
            ]
        );
        let last = line_segments(&song, 1, &config);
        assert_eq!((last[0].start_ms, last[0].end_ms), (20_000, 20_720));
    }

    #[test]
    fn schedules_follow_the_rate_and_start_mid_way() {
        let segment = Segment {
            text: "word".into(),
            start_ms: 10_000,
            end_ms: 12_000,
        };
        assert_eq!(segment.schedule(11_000.0, 1.0), (-1_000.0, 2_000.0));
        assert_eq!(segment.schedule(9_000.0, 2.0), (500.0, 1_000.0));
        assert_eq!(segment.schedule(9_000.0, f64::NAN), (1_000.0, 2_000.0));
        assert_eq!(segment.schedule(9_000.0, 100.0), (250.0, 500.0));
    }

    #[test]
    fn the_clock_reschedules_after_seeks_rate_changes_and_drift() {
        let mut clock = SyncClock::new(10_000.0, 1.0);
        assert!(!clock.observe(10_250.0, 1.0, 250.0, true));
        assert!(!clock.observe(10_250.0, 1.0, 5_000.0, false));
        assert!(!clock.observe(10_520.0, 1.0, 250.0, true));
        assert!(clock.observe(40_000.0, 1.0, 250.0, true));
        assert!(!clock.observe(40_250.0, 1.0, 250.0, true));
        let mut drifting = SyncClock::new(0.0, 1.0);
        let mut rescheduled = false;
        for step in 1..=20 {
            rescheduled |= drifting.observe(f64::from(step) * 280.0, 1.0, 250.0, true);
        }
        assert!(rescheduled);
        let mut faster = SyncClock::new(0.0, 1.0);
        assert!(faster.observe(250.0, 2.0, 250.0, true));
        assert!(!faster.observe(750.0, 2.0, 250.0, true));
    }
}
