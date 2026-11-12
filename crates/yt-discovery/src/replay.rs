//! «Replay» summaries of what the user heard over a period, and «En loop»:
//! the songs on repeat right now.
//!
//! A play is any listen that is not a skip under `ListenPolicy::default()`,
//! the policy every profile uses unless configured otherwise.

use crate::{artist_key, calendar::SECONDS_PER_DAY, listen::heard_seconds, ListenPolicy};
use std::collections::HashMap;
use yt_core::ListenSignal;

/// One song of a period's summary.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayEntry {
    /// The song's media id.
    pub media_id: String,
    /// Title of the latest listen in the period.
    pub title: String,
    /// Artist of the latest listen in the period.
    pub artist: String,
    /// Media seconds heard over the period, skips included.
    pub heard_seconds: f64,
    /// Listens that were not skips.
    pub plays: u32,
}

/// Up to `limit` songs heard from `since` (inclusive) to `until`
/// (exclusive), both Unix seconds, most heard seconds first, then most
/// plays, then by id. A play is a listen that is not a skip under
/// `ListenPolicy::default()`; songs that were only skipped are left out. An
/// empty or invalid period gives nothing.
pub fn replay(signals: &[ListenSignal], since: f64, until: f64, limit: usize) -> Vec<ReplayEntry> {
    let policy = ListenPolicy::default();
    let mut songs: HashMap<&str, (ReplayEntry, f64)> = HashMap::new();
    for signal in signals
        .iter()
        .filter(|signal| (since..until).contains(&signal.started_at))
    {
        let (entry, latest) = songs.entry(signal.media_id.as_str()).or_insert_with(|| {
            let entry = ReplayEntry {
                media_id: signal.media_id.clone(),
                title: signal.title.clone(),
                artist: signal.artist.clone(),
                heard_seconds: 0.0,
                plays: 0,
            };
            (entry, signal.started_at)
        });
        entry.heard_seconds += heard_seconds(signal);
        if policy.is_play(signal) {
            entry.plays = entry.plays.saturating_add(1);
        }
        if signal.started_at > *latest {
            *latest = signal.started_at;
            entry.title.clone_from(&signal.title);
            entry.artist.clone_from(&signal.artist);
        }
    }
    let mut entries: Vec<ReplayEntry> = songs
        .into_values()
        .map(|(entry, _)| entry)
        .filter(|entry| entry.plays > 0)
        .collect();
    entries.sort_by(|left, right| {
        right
            .heard_seconds
            .total_cmp(&left.heard_seconds)
            .then(right.plays.cmp(&left.plays))
            .then_with(|| left.media_id.cmp(&right.media_id))
    });
    entries.truncate(limit);
    entries
}

struct ArtistTime {
    name: String,
    latest: f64,
    seconds: f64,
}

/// Up to `limit` artists (grouped by `artist_key`) by media seconds heard
/// from `since` (inclusive) to `until` (exclusive), skips included, most
/// first, with the display name of their latest listen; ties break by name.
/// Artists with nothing heard are left out.
pub fn top_artists_by_time(
    signals: &[ListenSignal],
    since: f64,
    until: f64,
    limit: usize,
) -> Vec<(String, f64)> {
    let mut artists: HashMap<String, ArtistTime> = HashMap::new();
    for signal in signals
        .iter()
        .filter(|signal| (since..until).contains(&signal.started_at))
    {
        let key = artist_key(&signal.artist);
        if key.is_empty() {
            continue;
        }
        let artist = artists.entry(key).or_insert_with(|| ArtistTime {
            name: signal.artist.clone(),
            latest: signal.started_at,
            seconds: 0.0,
        });
        artist.seconds += heard_seconds(signal);
        if signal.started_at > artist.latest {
            artist.latest = signal.started_at;
            artist.name.clone_from(&signal.artist);
        }
    }
    let mut ranked: Vec<(String, f64)> = artists
        .into_values()
        .filter(|artist| artist.seconds > 0.0)
        .map(|artist| (artist.name, artist.seconds))
        .collect();
    ranked.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    ranked.truncate(limit);
    ranked
}

/// «En loop»: up to `limit` songs played at least `min_plays` times (at
/// least once) in the `window_days` before `now` (Unix seconds), most plays
/// first, then the most recently played, then by id. Skips under
/// `ListenPolicy::default()` never count, nor do listens after `now`.
pub fn on_repeat(
    signals: &[ListenSignal],
    now: f64,
    window_days: f64,
    min_plays: u32,
    limit: usize,
) -> Vec<String> {
    let policy = ListenPolicy::default();
    let since = now - window_days.max(0.0) * SECONDS_PER_DAY;
    let mut plays: HashMap<&str, (u32, f64)> = HashMap::new();
    for signal in signals
        .iter()
        .filter(|signal| (since..=now).contains(&signal.started_at) && policy.is_play(signal))
    {
        let (count, latest) = plays
            .entry(signal.media_id.as_str())
            .or_insert((0, signal.started_at));
        *count = count.saturating_add(1);
        *latest = latest.max(signal.started_at);
    }
    let mut ranked: Vec<(&str, u32, f64)> = plays
        .into_iter()
        .filter(|(_, (count, _))| *count >= min_plays.max(1))
        .map(|(media_id, (count, latest))| (media_id, count, latest))
        .collect();
    ranked.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then(right.2.total_cmp(&left.2))
            .then_with(|| left.0.cmp(right.0))
    });
    ranked
        .into_iter()
        .take(limit)
        .map(|(media_id, _, _)| media_id.to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{completed, skipped, DAY, HOUR, NOW};

    fn history() -> Vec<ListenSignal> {
        let mut long = completed("long", "Slow Band", NOW - 2.0 * DAY);
        long.listened_seconds = 900.0;
        long.duration = 900.0;
        let mut renamed = completed("often", "Band VEVO", NOW - 3.0 * DAY);
        renamed.title = "Often (Official Video)".into();
        vec![
            completed("often", "Band", NOW - 5.0 * DAY),
            completed("often", "Band", NOW - 4.0 * DAY),
            renamed,
            completed("often", "Band", NOW - 40.0 * DAY),
            long,
            completed("twice", "Duo", NOW - 6.0 * DAY),
            completed("twice", "Duo", NOW - HOUR),
            skipped("skipped", "Band", NOW - DAY),
            skipped("skipped", "Band", NOW - 2.0 * DAY),
            skipped("skipped", "Band", NOW - 3.0 * DAY),
            completed("future", "Band", NOW + HOUR),
        ]
    }

    #[test]
    fn replay_orders_by_heard_time_then_plays_and_drops_pure_skips() {
        let entries = replay(&history(), NOW - 30.0 * DAY, NOW, 10);
        let ids: Vec<&str> = entries
            .iter()
            .map(|entry| entry.media_id.as_str())
            .collect();
        assert_eq!(ids, ["long", "often", "twice"]);
        let often = &entries[1];
        assert_eq!(often.plays, 3);
        assert_eq!(often.heard_seconds, 600.0);
        assert_eq!(often.title, "Often (Official Video)");
        assert_eq!(often.artist, "Band VEVO");
        assert_eq!(replay(&history(), NOW - 30.0 * DAY, NOW, 1).len(), 1);
        assert!(replay(&history(), NOW, NOW - DAY, 10).is_empty());
        assert!(replay(&history(), f64::NAN, NOW, 10).is_empty());
    }

    #[test]
    fn artists_rank_by_time_heard_under_their_latest_name() {
        let artists = top_artists_by_time(&history(), NOW - 30.0 * DAY, NOW, 10);
        assert_eq!(artists[0], ("Slow Band".to_owned(), 900.0));
        // «Band» and «Band VEVO» are one artist: 600 s of plays plus 15 s of skips.
        assert_eq!(artists[1], ("Band".to_owned(), 615.0));
        assert_eq!(artists[2], ("Duo".to_owned(), 400.0));
        assert_eq!(artists.len(), 3);
    }

    #[test]
    fn on_repeat_counts_recent_plays_but_never_skips() {
        let looped = on_repeat(&history(), NOW, 7.0, 2, 10);
        assert_eq!(looped, ["often", "twice"]);
        assert_eq!(on_repeat(&history(), NOW, 7.0, 0, 10).len(), 3);
        assert_eq!(on_repeat(&history(), NOW, 7.0, 3, 10), ["often"]);
        assert_eq!(on_repeat(&history(), NOW, 4.5, 2, 10), ["often"]);
        assert!(on_repeat(&history(), NOW, 7.0, 2, 0).is_empty());
    }
}
