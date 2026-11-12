//! Builders shared by the unit tests.

use crate::{rng::SeededRng, Candidate};
use yt_core::{ListenEnd, ListenSignal, ListenSource};

pub(crate) const HOUR: f64 = 3_600.0;
pub(crate) const DAY: f64 = 86_400.0;
/// A fixed instant: 2026-11-11 00:00 UTC, a Wednesday.
pub(crate) const NOW: f64 = 1_794_355_200.0;
/// Length of every synthetic song, in seconds.
pub(crate) const SONG_SECONDS: f64 = 200.0;

/// A listen to the end of song `id` («Song id» by `artist`).
pub(crate) fn completed(id: &str, artist: &str, started_at: f64) -> ListenSignal {
    ListenSignal {
        media_id: id.into(),
        title: format!("Song {id}"),
        artist: artist.into(),
        remote: false,
        source: ListenSource::User,
        started_at,
        listened_seconds: SONG_SECONDS,
        duration: SONG_SECONDS,
        end: ListenEnd::Finished,
    }
}

/// A listen abandoned after five seconds.
pub(crate) fn skipped(id: &str, artist: &str, started_at: f64) -> ListenSignal {
    ListenSignal {
        listened_seconds: 5.0,
        end: ListenEnd::Interrupted,
        ..completed(id, artist, started_at)
    }
}

/// A song-length candidate titled «Song id».
pub(crate) fn candidate(id: &str, artist: &str, provider_rank: usize) -> Candidate {
    Candidate {
        media_id: id.into(),
        title: format!("Song {id}"),
        artist: artist.into(),
        duration: SONG_SECONDS,
        provider_rank,
    }
}

/// Complete listens of `groups`, each group heard in its own sessions in a
/// shuffled order, so songs of one group co-occur and never meet another
/// group's. Listens are back to back within a session and sessions are hours
/// apart; the last listen ends before `end`.
pub(crate) fn grouped_history(
    groups: &[Vec<(String, String)>],
    sessions_per_group: usize,
    end: f64,
    seed: u64,
) -> Vec<ListenSignal> {
    let mut rng = SeededRng::new(seed);
    let mut order: Vec<&[(String, String)]> = Vec::new();
    for _ in 0..sessions_per_group {
        order.extend(groups.iter().map(Vec::as_slice));
    }
    let total: usize = order.iter().map(|group| group.len()).sum();
    let mut clock = end - total as f64 * SONG_SECONDS - order.len() as f64 * 2.0 * HOUR;
    let mut signals = Vec::new();
    for group in order {
        let mut songs = group.to_vec();
        for index in (1..songs.len()).rev() {
            let other = (rng.next_f64() * (index + 1) as f64) as usize;
            songs.swap(index, other.min(index));
        }
        for (id, artist) in songs {
            signals.push(completed(&id, &artist, clock));
            clock += SONG_SECONDS;
        }
        clock += 2.0 * HOUR;
    }
    signals
}

/// `count` songs `prefix0…` whose artists come from `artist(index)`.
pub(crate) fn group(
    prefix: &str,
    count: usize,
    artist: impl Fn(usize) -> String,
) -> Vec<(String, String)> {
    (0..count)
        .map(|index| (format!("{prefix}{index}"), artist(index)))
        .collect()
}
