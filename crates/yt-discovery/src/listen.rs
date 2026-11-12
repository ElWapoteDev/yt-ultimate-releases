//! Measuring what was actually heard and judging how a listen went.

use yt_core::{ListenEnd, ListenSignal};

/// Slack added to the largest plausible advance between two samples, in
/// seconds; absorbs scheduler jitter without counting a seek as listening.
const SAMPLE_SLACK_SECONDS: f64 = 0.5;
/// Factor over the expected advance accepted before a jump counts as a seek.
const SAMPLE_TOLERANCE: f64 = 1.5;

/// Accumulates media time actually heard from periodic position samples.
/// Seeking forward or backward is never counted as listening.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListenMeter {
    heard: f64,
    last_position: Option<f64>,
}

impl ListenMeter {
    /// Feed one position sample (media seconds). `elapsed_seconds` is the wall
    /// time since the previous sample and `rate` the playback speed.
    pub fn observe(&mut self, position: f64, playing: bool, elapsed_seconds: f64, rate: f64) {
        if !position.is_finite() || position < 0.0 {
            return;
        }
        if let Some(last) = self.last_position {
            let advance = position - last;
            let expected = elapsed_seconds.max(0.0) * rate.max(0.0);
            let plausible = expected * SAMPLE_TOLERANCE + SAMPLE_SLACK_SECONDS;
            if playing && advance > 0.0 && advance <= plausible {
                self.heard += advance;
            }
        }
        self.last_position = Some(position);
    }

    /// Media seconds heard so far.
    pub fn heard_seconds(&self) -> f64 {
        self.heard
    }
}

/// Thresholds that turn listening facts into an outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct ListenPolicy {
    /// Share of the duration that counts as a complete listen.
    pub complete_ratio: f64,
    /// An interrupted listen shorter than this many seconds…
    pub skip_seconds: f64,
    /// …and below this share of the duration is a skip.
    pub skip_ratio: f64,
    /// Share assumed for a meaningful listen whose duration is unknown.
    pub unknown_duration_ratio: f64,
    /// A natural end counts as complete only when at least this share was
    /// heard; seeking to the last seconds is not a full listen.
    pub finished_min_ratio: f64,
}

impl Default for ListenPolicy {
    fn default() -> Self {
        Self {
            complete_ratio: 0.8,
            skip_seconds: 30.0,
            skip_ratio: 0.5,
            unknown_duration_ratio: 0.5,
            finished_min_ratio: 0.5,
        }
    }
}

/// How a single listen went.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ListenOutcome {
    /// Heard to the end, or nearly.
    Completed,
    /// Heard this share (0..1) of the media before moving on.
    Partial(f64),
    /// Abandoned early.
    Skipped,
    /// A play whose length was never recorded (imported history).
    Played,
    /// Still playing; carries the share heard so far.
    Ongoing(f64),
}

impl ListenPolicy {
    /// Judge one recorded listen.
    pub fn classify(&self, signal: &ListenSignal) -> ListenOutcome {
        let ratio = if signal.duration > 0.0 {
            (signal.listened_seconds / signal.duration).clamp(0.0, 1.0)
        } else if signal.listened_seconds >= self.skip_seconds {
            self.unknown_duration_ratio
        } else {
            0.0
        };
        match signal.end {
            ListenEnd::Unknown => ListenOutcome::Played,
            ListenEnd::Finished if signal.duration <= 0.0 || ratio >= self.finished_min_ratio => {
                ListenOutcome::Completed
            }
            ListenEnd::Finished => ListenOutcome::Partial(ratio),
            ListenEnd::Playing => ListenOutcome::Ongoing(ratio),
            ListenEnd::Interrupted if ratio >= self.complete_ratio => ListenOutcome::Completed,
            ListenEnd::Interrupted
                if signal.listened_seconds < self.skip_seconds && ratio < self.skip_ratio =>
            {
                ListenOutcome::Skipped
            }
            ListenEnd::Interrupted => ListenOutcome::Partial(ratio),
        }
    }

    /// Whether a listen counts as a play: anything but a skip.
    pub(crate) fn is_play(&self, signal: &ListenSignal) -> bool {
        !matches!(self.classify(signal), ListenOutcome::Skipped)
    }
}

/// Media seconds a listen actually played; invalid values count as none.
pub(crate) fn heard_seconds(signal: &ListenSignal) -> f64 {
    if signal.listened_seconds.is_finite() {
        signal.listened_seconds.max(0.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yt_core::ListenSource;

    fn signal(listened_seconds: f64, duration: f64, end: ListenEnd) -> ListenSignal {
        ListenSignal {
            media_id: "id".into(),
            title: "Song".into(),
            artist: "Artist".into(),
            remote: false,
            source: ListenSource::User,
            started_at: 0.0,
            listened_seconds,
            duration,
            end,
        }
    }

    #[test]
    fn meter_counts_playback_but_not_seeks_or_pauses() {
        let mut meter = ListenMeter::default();
        meter.observe(0.0, true, 0.25, 1.0);
        for step in 1..=4 {
            meter.observe(step as f64 * 0.25, true, 0.25, 1.0);
        }
        assert!((meter.heard_seconds() - 1.0).abs() < 1e-9);
        meter.observe(60.0, true, 0.25, 1.0);
        meter.observe(10.0, true, 0.25, 1.0);
        meter.observe(10.25, false, 0.25, 1.0);
        assert!((meter.heard_seconds() - 1.0).abs() < 1e-9);
        meter.observe(10.75, true, 0.25, 2.0);
        assert!((meter.heard_seconds() - 1.5).abs() < 1e-9);
        meter.observe(f64::NAN, true, 0.25, 1.0);
        assert!((meter.heard_seconds() - 1.5).abs() < 1e-9);
    }

    #[test]
    fn listens_are_classified_from_raw_facts() {
        let policy = ListenPolicy::default();
        assert_eq!(
            policy.classify(&signal(120.0, 200.0, ListenEnd::Finished)),
            ListenOutcome::Completed
        );
        assert_eq!(
            policy.classify(&signal(16.0, 200.0, ListenEnd::Finished)),
            ListenOutcome::Partial(0.08),
            "seeking to the end and letting it finish is not a full listen"
        );
        assert_eq!(
            policy.classify(&signal(16.0, 0.0, ListenEnd::Finished)),
            ListenOutcome::Completed
        );
        assert_eq!(
            policy.classify(&signal(170.0, 200.0, ListenEnd::Interrupted)),
            ListenOutcome::Completed
        );
        assert_eq!(
            policy.classify(&signal(12.0, 200.0, ListenEnd::Interrupted)),
            ListenOutcome::Skipped
        );
        assert_eq!(
            policy.classify(&signal(100.0, 200.0, ListenEnd::Interrupted)),
            ListenOutcome::Partial(0.5)
        );
        assert_eq!(
            policy.classify(&signal(0.0, 0.0, ListenEnd::Unknown)),
            ListenOutcome::Played
        );
        assert_eq!(
            policy.classify(&signal(45.0, 0.0, ListenEnd::Interrupted)),
            ListenOutcome::Partial(0.5)
        );
        assert_eq!(
            policy.classify(&signal(20.0, 30.0, ListenEnd::Interrupted)),
            ListenOutcome::Partial(20.0 / 30.0)
        );
        assert_eq!(
            policy.classify(&signal(50.0, 100.0, ListenEnd::Playing)),
            ListenOutcome::Ongoing(0.5)
        );
    }
}
