//! Listening events: what the user played, for how long, and how it ended.
//!
//! Events store raw facts (listened seconds, duration, end reason). How those
//! facts become affinity is policy owned by `yt-discovery`, so it can change
//! without rewriting stored data.

use crate::{
    AppCore, CoreError, MediaItem, RemoteMedia, Result,
    media::{MAX_DURATION_SECONDS, prune_orphan_remote_media, remote_by_id, upsert_remote},
    now,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

/// Events kept; the oldest are pruned on write. Roughly two years of an
/// hour of daily listening.
pub const LISTENING_EVENT_LIMIT: usize = 5_000;

/// Who chose the media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListenSource {
    /// Picked by the user: library, search, playlists, queue, home.
    User,
    /// Picked by autoplay after the queue ran out.
    Autoplay,
}

/// How a listening event ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListenEnd {
    /// Still playing.
    Playing,
    /// Reached the natural end of the media.
    Finished,
    /// The user moved on, stopped, or the application closed.
    Interrupted,
    /// Imported from history that only recorded that a play happened.
    Unknown,
}

impl ListenSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Autoplay => "autoplay",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "autoplay" => Some(Self::Autoplay),
            _ => None,
        }
    }
}

impl ListenEnd {
    fn as_str(self) -> &'static str {
        match self {
            Self::Playing => "playing",
            Self::Finished => "finished",
            Self::Interrupted => "interrupted",
            Self::Unknown => "unknown",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "playing" => Some(Self::Playing),
            "finished" => Some(Self::Finished),
            "interrupted" => Some(Self::Interrupted),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// Handle of an open listening event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListenId(i64);

/// What is being listened to.
#[derive(Debug, Clone, Copy)]
pub enum ListenTarget<'a> {
    /// A library track id.
    Local(&'a str),
    /// Online media, remembered as part of the same write.
    Remote(&'a RemoteMedia),
}

/// Measured progress of one event, in seconds of media time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ListenProgress {
    /// Seconds actually heard; seeking forward does not count.
    pub listened_seconds: f64,
    /// Media duration; 0 keeps the stored duration.
    pub duration: f64,
}

impl ListenProgress {
    fn validate(self) -> Result<()> {
        let valid = |value: f64| value.is_finite() && (0.0..=MAX_DURATION_SECONDS).contains(&value);
        if valid(self.listened_seconds) && valid(self.duration) {
            Ok(())
        } else {
            Err(CoreError::Invalid("Invalid listening progress".into()))
        }
    }
}

/// One listening event with the display metadata ranking needs.
#[derive(Debug, Clone, PartialEq)]
pub struct ListenSignal {
    pub media_id: String,
    pub title: String,
    pub artist: String,
    pub remote: bool,
    pub source: ListenSource,
    /// Unix seconds.
    pub started_at: f64,
    pub listened_seconds: f64,
    pub duration: f64,
    pub end: ListenEnd,
}

/// What an erasure removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ListeningErasure {
    pub events: usize,
    pub cached_lists: usize,
    pub remote_media: usize,
    /// Remembered album cover lookups.
    pub album_artwork: usize,
    /// Sound measurements of online media; library files keep theirs, which
    /// describe the files rather than the listening.
    pub audio_features: usize,
    /// Remembered searches.
    pub searches: usize,
    /// DJ analyses of online media; library files keep theirs, like their
    /// sound measurements.
    pub dj_analyses: usize,
}

fn prune_events(db: &Connection) -> Result<usize> {
    let removed = db.execute(
        "DELETE FROM rust_listening_events WHERE id IN (
            SELECT id FROM rust_listening_events ORDER BY started_at DESC,id DESC LIMIT -1 OFFSET ?1)",
        [LISTENING_EVENT_LIMIT as i64],
    )?;
    if removed > 0 {
        prune_orphan_remote_media(db)?;
    }
    Ok(removed)
}

impl AppCore {
    /// Open a listening event. Any event still marked as playing is closed as
    /// interrupted first: only one thing plays at a time.
    pub fn begin_listen(&self, target: ListenTarget<'_>, source: ListenSource) -> Result<ListenId> {
        let mut db = self.connection()?;
        let transaction = db.transaction()?;
        let (media_id, duration) = match target {
            ListenTarget::Local(id) => {
                let duration: Option<f64> = transaction
                    .query_row("SELECT duration FROM tracks WHERE id=?1", [id], |row| {
                        row.get(0)
                    })
                    .ok();
                (
                    id.to_owned(),
                    duration.ok_or_else(|| CoreError::NotFound(id.into()))?,
                )
            }
            ListenTarget::Remote(media) => {
                upsert_remote(&transaction, media)?;
                (media.id.clone(), media.duration)
            }
        };
        let at = now();
        transaction.execute(
            "UPDATE rust_listening_events SET end_reason='interrupted',updated_at=?1 WHERE end_reason='playing'",
            [at],
        )?;
        transaction.execute(
            "INSERT INTO rust_listening_events(media_id,source,started_at,updated_at,listened_seconds,duration,end_reason)
             VALUES(?1,?2,?3,?3,0,?4,'playing')",
            params![media_id, source.as_str(), at, duration.max(0.0)],
        )?;
        let id = ListenId(transaction.last_insert_rowid());
        prune_events(&transaction)?;
        transaction.commit()?;
        Ok(id)
    }

    /// Record progress of an open event; closed or pruned events are left
    /// unchanged, so late writes are harmless.
    pub fn record_listen_progress(&self, id: ListenId, progress: ListenProgress) -> Result<()> {
        progress.validate()?;
        self.connection()?.execute(
            "UPDATE rust_listening_events SET listened_seconds=?1,
                duration=CASE WHEN ?2>0 THEN ?2 ELSE duration END,updated_at=?3
             WHERE id=?4 AND end_reason='playing'",
            params![progress.listened_seconds, progress.duration, now(), id.0],
        )?;
        Ok(())
    }

    /// Close an open event as finished or interrupted.
    pub fn end_listen(&self, id: ListenId, progress: ListenProgress, end: ListenEnd) -> Result<()> {
        progress.validate()?;
        if !matches!(end, ListenEnd::Finished | ListenEnd::Interrupted) {
            return Err(CoreError::Invalid(
                "A listening event ends as finished or interrupted".into(),
            ));
        }
        self.connection()?.execute(
            "UPDATE rust_listening_events SET listened_seconds=?1,
                duration=CASE WHEN ?2>0 THEN ?2 ELSE duration END,updated_at=?3,end_reason=?4
             WHERE id=?5 AND end_reason='playing'",
            params![
                progress.listened_seconds,
                progress.duration,
                now(),
                end.as_str(),
                id.0
            ],
        )?;
        Ok(())
    }

    /// Newest events first, with the title and artist of media that still
    /// exists. Rows with unknown enum values are skipped.
    pub fn listening_signals(&self, limit: usize) -> Result<Vec<ListenSignal>> {
        let db = self.connection()?;
        let mut query = db.prepare(
            "SELECT e.media_id,e.source,e.started_at,e.listened_seconds,e.duration,e.end_reason,
                    COALESCE(t.title,r.title) AS title,COALESCE(t.artist,r.artist) AS artist,
                    t.id IS NULL AS remote
             FROM rust_listening_events e
             LEFT JOIN tracks t ON t.id=e.media_id
             LEFT JOIN rust_remote_media r ON r.id=e.media_id
             WHERE t.id IS NOT NULL OR r.id IS NOT NULL
             ORDER BY e.started_at DESC,e.id DESC LIMIT ?1",
        )?;
        let rows = query.query_map([limit.min(LISTENING_EVENT_LIMIT) as i64], |row| {
            Ok((
                row.get::<_, String>("source")?,
                row.get::<_, String>("end_reason")?,
                ListenSignal {
                    media_id: row.get("media_id")?,
                    title: row.get("title")?,
                    artist: row.get("artist")?,
                    remote: row.get("remote")?,
                    source: ListenSource::User,
                    started_at: row.get("started_at")?,
                    listened_seconds: row.get("listened_seconds")?,
                    duration: row.get("duration")?,
                    end: ListenEnd::Unknown,
                },
            ))
        })?;
        let mut signals = Vec::new();
        for row in rows {
            let (source, end, signal) = row?;
            if let (Some(source), Some(end)) =
                (ListenSource::parse(&source), ListenEnd::parse(&end))
            {
                signals.push(ListenSignal {
                    source,
                    end,
                    ..signal
                });
            }
        }
        Ok(signals)
    }

    /// Distinct media in order of their latest play, newest first. Library
    /// tracks whose file is missing are skipped because they cannot play.
    pub fn recently_played(&self, limit: usize) -> Result<Vec<MediaItem>> {
        let candidates: Vec<String> = {
            let db = self.connection()?;
            let mut query = db.prepare(
                "SELECT media_id FROM rust_listening_events GROUP BY media_id
                 ORDER BY MAX(started_at) DESC LIMIT ?1",
            )?;
            // Read extra ids so skipped entries do not shorten the list.
            query
                .query_map(
                    [(limit.saturating_mul(2)).min(LISTENING_EVENT_LIMIT) as i64],
                    |row| row.get(0),
                )?
                .collect::<std::result::Result<_, _>>()?
        };
        let mut items = Vec::new();
        for id in candidates {
            if items.len() >= limit {
                break;
            }
            if RemoteMedia::is_remote_id(&id) {
                if let Some(media) = remote_by_id(&*self.connection()?, &id)? {
                    items.push(MediaItem::Remote(media));
                }
            } else if let Ok(track) = self.track(&id)
                && !track.missing
            {
                items.push(MediaItem::Local(track));
            }
        }
        Ok(items)
    }

    /// Erase the listening history and everything derived from it: events,
    /// cached provider responses, remembered album cover lookups, remembered
    /// online media that is not a favorite, and the audio measurements taken
    /// of online media while it played. Favorites, followed artists, library
    /// measurements and library covers are kept.
    pub fn clear_listening_history(&self) -> Result<ListeningErasure> {
        let mut db = self.connection()?;
        let transaction = db.transaction()?;
        let events = transaction.execute("DELETE FROM rust_listening_events", [])?;
        let cached_lists = transaction.execute("DELETE FROM rust_provider_cache", [])?;
        let album_artwork = transaction.execute("DELETE FROM rust_album_artwork", [])?;
        let audio_features = transaction.execute(
            "DELETE FROM rust_audio_features WHERE substr(media_id,1,length(?1))=?1",
            [crate::REMOTE_ID_PREFIX],
        )?;
        let searches = transaction.execute("DELETE FROM rust_search_history", [])?;
        let dj_analyses = transaction.execute(
            "DELETE FROM rust_dj_analysis WHERE substr(media_id,1,length(?1))=?1",
            [crate::REMOTE_ID_PREFIX],
        )?;
        let remote_media = prune_orphan_remote_media(&transaction)?;
        transaction.commit()?;
        Ok(ListeningErasure {
            events,
            cached_lists,
            remote_media,
            album_artwork,
            audio_features,
            searches,
            dj_analyses,
        })
    }

    /// Events left open by a previous run end as interrupted.
    pub(crate) fn recover_interrupted_listens(&self) -> Result<usize> {
        Ok(self.connection()?.execute(
            "UPDATE rust_listening_events SET end_reason='interrupted',updated_at=?1 WHERE end_reason='playing'",
            [now()],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(listened_seconds: f64, duration: f64) -> ListenProgress {
        ListenProgress {
            listened_seconds,
            duration,
        }
    }

    #[test]
    fn events_record_facts_and_only_one_stays_open() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let track = core.import_bytes("song.wav", &[1, 2, 3, 4]).unwrap();
        let online = RemoteMedia::from_parts("aqz-KE-bpKQ", "Online", "Artist", "", 180.0).unwrap();
        let first = core
            .begin_listen(ListenTarget::Local(&track.id), ListenSource::User)
            .unwrap();
        core.record_listen_progress(first, progress(12.0, 0.0))
            .unwrap();
        let second = core
            .begin_listen(ListenTarget::Remote(&online), ListenSource::Autoplay)
            .unwrap();
        core.end_listen(second, progress(170.0, 180.0), ListenEnd::Finished)
            .unwrap();
        core.end_listen(first, progress(99.0, 0.0), ListenEnd::Finished)
            .unwrap();
        let signals = core.listening_signals(10).unwrap();
        assert_eq!(signals.len(), 2);
        assert_eq!(signals[0].media_id, online.id);
        assert!(signals[0].remote);
        assert_eq!(signals[0].source, ListenSource::Autoplay);
        assert_eq!(signals[0].end, ListenEnd::Finished);
        assert_eq!(signals[1].end, ListenEnd::Interrupted);
        assert_eq!(signals[1].listened_seconds, 12.0);
        assert_eq!(signals[1].title, track.title);
    }

    #[test]
    fn invalid_progress_and_unknown_media_are_rejected() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        assert!(matches!(
            core.begin_listen(ListenTarget::Local("missing"), ListenSource::User),
            Err(CoreError::NotFound(_))
        ));
        let track = core.import_bytes("song.wav", &[1, 2, 3, 4]).unwrap();
        let id = core
            .begin_listen(ListenTarget::Local(&track.id), ListenSource::User)
            .unwrap();
        for bad in [
            progress(f64::NAN, 0.0),
            progress(-1.0, 0.0),
            progress(0.0, f64::INFINITY),
        ] {
            assert!(core.record_listen_progress(id, bad).is_err());
        }
        assert!(
            core.end_listen(id, progress(1.0, 0.0), ListenEnd::Playing)
                .is_err()
        );
    }

    #[test]
    fn recently_played_is_distinct_newest_first_and_mixes_sources() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let track = core.import_bytes("song.wav", &[1, 2, 3, 4]).unwrap();
        let online = RemoteMedia::from_parts("abcdefghijk", "Online", "", "", 0.0).unwrap();
        for target in [
            ListenTarget::Local(&track.id),
            ListenTarget::Remote(&online),
            ListenTarget::Local(&track.id),
        ] {
            core.begin_listen(target, ListenSource::User).unwrap();
        }
        let recent = core.recently_played(10).unwrap();
        assert_eq!(
            recent.iter().map(MediaItem::id).collect::<Vec<_>>(),
            vec![track.id.as_str(), online.id.as_str()]
        );
        assert_eq!(core.recently_played(1).unwrap().len(), 1);
    }

    #[test]
    fn erasure_keeps_favorites_and_forgets_everything_derived() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let kept = RemoteMedia::from_parts("abcdefghijk", "Kept", "", "", 0.0).unwrap();
        let forgotten = RemoteMedia::from_parts("lmnopqrstuv", "Forgotten", "", "", 0.0).unwrap();
        core.set_remote_favorite(&kept, true).unwrap();
        core.begin_listen(ListenTarget::Remote(&kept), ListenSource::User)
            .unwrap();
        core.begin_listen(ListenTarget::Remote(&forgotten), ListenSource::User)
            .unwrap();
        core.store_discovery("mix:abcdefghijk", std::slice::from_ref(&forgotten))
            .unwrap();
        let track = core.import_bytes("song.wav", &[1, 2, 3, 4]).unwrap();
        let features = crate::AudioFeatures {
            analyzed_seconds: 30.0,
            energy: 0.5,
            dynamics: 0.5,
            brightness: 0.5,
            onset_rate: 0.5,
            tempo_bpm: 120.0,
            mode: 0.5,
        };
        core.store_audio_features(&forgotten.id, &features).unwrap();
        core.store_audio_features(&track.id, &features).unwrap();
        core.record_search("Peso Pluma").unwrap();
        let erased = core.clear_listening_history().unwrap();
        assert_eq!(
            erased,
            ListeningErasure {
                events: 2,
                cached_lists: 1,
                remote_media: 1,
                album_artwork: 0,
                audio_features: 1,
                searches: 1,
                dj_analyses: 0,
            }
        );
        assert!(core.listening_signals(10).unwrap().is_empty());
        assert!(core.remote_media(&kept.id).unwrap().is_some());
        assert!(core.remote_media(&forgotten.id).unwrap().is_none());
        assert_eq!(core.favorites().unwrap().len(), 1);
        let kept_features = core.audio_features().unwrap();
        assert_eq!(kept_features.keys().collect::<Vec<_>>(), [&track.id]);
        assert!(core.search_history(10).unwrap().is_empty());
    }

    #[test]
    fn open_events_from_a_previous_run_end_interrupted() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let track = core.import_bytes("song.wav", &[1, 2, 3, 4]).unwrap();
        core.begin_listen(ListenTarget::Local(&track.id), ListenSource::User)
            .unwrap();
        drop(core);
        let reopened = AppCore::open(profile.path()).unwrap();
        assert_eq!(
            reopened.listening_signals(1).unwrap()[0].end,
            ListenEnd::Interrupted
        );
    }

    #[test]
    fn events_are_bounded() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let track = core.import_bytes("song.wav", &[1, 2, 3, 4]).unwrap();
        {
            let db = core.connection().unwrap();
            for index in 0..LISTENING_EVENT_LIMIT {
                db.execute(
                    "INSERT INTO rust_listening_events(media_id,source,started_at,updated_at,duration,end_reason)
                     VALUES(?1,'user',?2,?2,0,'finished')",
                    params![track.id, index as f64],
                )
                .unwrap();
            }
        }
        core.begin_listen(ListenTarget::Local(&track.id), ListenSource::User)
            .unwrap();
        let count: i64 = core
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM rust_listening_events", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, LISTENING_EVENT_LIMIT as i64);
    }
}
