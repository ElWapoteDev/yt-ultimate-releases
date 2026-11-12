//! Media identities shared by the library and online catalogs.
//!
//! A library track is identified by its track id. Online media is identified
//! by `youtube:<video id>` and stored only as display metadata: canonical
//! identity, title, artist, artwork and duration. Signed transport URLs and
//! credentials never belong in these records.

use crate::{AppCore, CoreError, Result, Track, now};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// Prefix of every remote media identifier.
pub const REMOTE_ID_PREFIX: &str = "youtube:";
/// Length of a YouTube video id.
pub const REMOTE_KEY_LENGTH: usize = 11;
/// Canonical watch URL prefix; the video id follows it.
const CANONICAL_WATCH_PREFIX: &str = "https://www.youtube.com/watch?v=";
/// Longest accepted title, in characters.
pub const MAX_TITLE_CHARS: usize = 512;
/// Longest accepted artist or channel name, in characters.
pub const MAX_ARTIST_CHARS: usize = 256;
/// Longest accepted artwork URL, in bytes.
pub const MAX_ARTWORK_URL_BYTES: usize = 2048;
/// Longest accepted media duration, in seconds (one week).
pub const MAX_DURATION_SECONDS: f64 = 7.0 * 86_400.0;

/// Display metadata of a playable online item that is not in the library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteMedia {
    /// `youtube:<video id>`.
    pub id: String,
    pub title: String,
    pub artist: String,
    /// `https` image URL, or empty when unknown.
    pub artwork_url: String,
    /// Seconds; 0 when unknown.
    pub duration: f64,
}

impl RemoteMedia {
    /// Build validated remote media from a provider video id and its display
    /// metadata. Text is trimmed; malformed input is rejected, never repaired.
    pub fn from_parts(
        video_id: &str,
        title: &str,
        artist: &str,
        artwork_url: &str,
        duration: f64,
    ) -> Result<Self> {
        let media = Self {
            id: format!("{REMOTE_ID_PREFIX}{video_id}"),
            title: title.trim().to_owned(),
            artist: artist.trim().to_owned(),
            artwork_url: artwork_url.trim().to_owned(),
            duration,
        };
        media.validate()?;
        Ok(media)
    }

    /// Whether `id` has the shape of a remote media identifier.
    pub fn is_remote_id(id: &str) -> bool {
        id.strip_prefix(REMOTE_ID_PREFIX).is_some_and(valid_key)
    }

    /// Provider video id (the part after the prefix).
    pub fn video_id(&self) -> &str {
        self.id.strip_prefix(REMOTE_ID_PREFIX).unwrap_or_default()
    }

    /// Canonical watch URL derived from the identity.
    pub fn source_url(&self) -> String {
        format!("{CANONICAL_WATCH_PREFIX}{}", self.video_id())
    }

    /// Check every invariant the store relies on.
    pub fn validate(&self) -> Result<()> {
        if !Self::is_remote_id(&self.id) {
            return Err(CoreError::Invalid("Invalid remote media identifier".into()));
        }
        if self.title.trim().is_empty() || !bounded_text(&self.title, MAX_TITLE_CHARS) {
            return Err(CoreError::Invalid("Invalid remote media title".into()));
        }
        if !bounded_text(&self.artist, MAX_ARTIST_CHARS) {
            return Err(CoreError::Invalid("Invalid remote media artist".into()));
        }
        if !valid_artwork_url(&self.artwork_url) {
            return Err(CoreError::Invalid("Invalid remote artwork URL".into()));
        }
        if !self.duration.is_finite() || !(0.0..=MAX_DURATION_SECONDS).contains(&self.duration) {
            return Err(CoreError::Invalid("Invalid remote media duration".into()));
        }
        Ok(())
    }
}

/// A playable item: a library track or remembered online media.
#[derive(Debug, Clone, PartialEq)]
pub enum MediaItem {
    Local(Track),
    Remote(RemoteMedia),
}

impl MediaItem {
    pub fn id(&self) -> &str {
        match self {
            Self::Local(track) => &track.id,
            Self::Remote(media) => &media.id,
        }
    }
    pub fn title(&self) -> &str {
        match self {
            Self::Local(track) => &track.title,
            Self::Remote(media) => &media.title,
        }
    }
    pub fn artist(&self) -> &str {
        match self {
            Self::Local(track) => &track.artist,
            Self::Remote(media) => &media.artist,
        }
    }
}

pub(crate) fn valid_key(key: &str) -> bool {
    key.len() == REMOTE_KEY_LENGTH
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub(crate) fn bounded_text(text: &str, max_chars: usize) -> bool {
    text.chars().count() <= max_chars && !text.chars().any(char::is_control)
}

pub(crate) fn valid_artwork_url(url: &str) -> bool {
    url.is_empty()
        || (url.starts_with("https://")
            && url.len() <= MAX_ARTWORK_URL_BYTES
            && !url
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control()))
}

const REMOTE_COLUMNS: &str = "id,title,artist,artwork_url,duration";

fn row_to_remote(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteMedia> {
    Ok(RemoteMedia {
        id: row.get("id")?,
        title: row.get("title")?,
        artist: row.get("artist")?,
        artwork_url: row.get("artwork_url")?,
        duration: row.get("duration")?,
    })
}

/// Insert or refresh remote metadata. A newer record never erases a known
/// artist, artwork or duration with an empty value.
pub(crate) fn upsert_remote(db: &Connection, media: &RemoteMedia) -> Result<()> {
    media.validate()?;
    db.execute(
        "INSERT INTO rust_remote_media(id,title,artist,artwork_url,duration,first_seen_at,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?6)
         ON CONFLICT(id) DO UPDATE SET
           title=excluded.title,
           artist=CASE WHEN excluded.artist<>'' THEN excluded.artist ELSE rust_remote_media.artist END,
           artwork_url=CASE WHEN excluded.artwork_url<>'' THEN excluded.artwork_url ELSE rust_remote_media.artwork_url END,
           duration=CASE WHEN excluded.duration>0 THEN excluded.duration ELSE rust_remote_media.duration END,
           updated_at=excluded.updated_at",
        params![
            media.id,
            media.title,
            media.artist,
            media.artwork_url,
            media.duration,
            now()
        ],
    )?;
    Ok(())
}

/// Read one remote record; a corrupt row reads as absent.
pub(crate) fn remote_by_id(db: &Connection, id: &str) -> Result<Option<RemoteMedia>> {
    let media = db
        .query_row(
            &format!("SELECT {REMOTE_COLUMNS} FROM rust_remote_media WHERE id=?1"),
            [id],
            row_to_remote,
        )
        .optional()?;
    Ok(media.filter(|media| media.validate().is_ok()))
}

/// Remote records referenced by nothing are personal data with no purpose;
/// measurements of online media go with them.
pub(crate) fn prune_orphan_remote_media(db: &Connection) -> Result<usize> {
    let removed = db.execute(
        "DELETE FROM rust_remote_media
         WHERE id NOT IN (SELECT media_id FROM rust_favorites)
           AND id NOT IN (SELECT media_id FROM rust_listening_events)",
        [],
    )?;
    db.execute(
        "DELETE FROM rust_audio_features
         WHERE media_id LIKE 'youtube:%' AND media_id NOT IN (SELECT id FROM rust_remote_media)",
        [],
    )?;
    Ok(removed)
}

/// Whether a library track or remembered remote item has this id.
pub(crate) fn media_exists(db: &Connection, id: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM tracks WHERE id=?1)
             OR EXISTS(SELECT 1 FROM rust_remote_media WHERE id=?1)",
        [id],
        |row| row.get(0),
    )?)
}

pub(crate) fn remote_rows(
    statement: &mut rusqlite::Statement<'_>,
    parameters: impl rusqlite::Params,
) -> Result<Vec<(RemoteMedia, f64)>> {
    let rows = statement.query_map(parameters, |row| Ok((row_to_remote(row)?, row.get(5)?)))?;
    let mut items = Vec::new();
    for row in rows {
        let (media, extra) = row?;
        if media.validate().is_ok() {
            items.push((media, extra));
        }
    }
    Ok(items)
}

impl AppCore {
    /// Remember online media the user interacted with so it can be shown
    /// again (history, favorites) without asking the provider.
    pub fn remember_remote(&self, media: &RemoteMedia) -> Result<()> {
        upsert_remote(&*self.connection()?, media)
    }

    /// Remembered metadata for `id`, if any.
    pub fn remote_media(&self, id: &str) -> Result<Option<RemoteMedia>> {
        remote_by_id(&*self.connection()?, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RemoteMedia {
        RemoteMedia::from_parts(
            "aqz-KE-bpKQ",
            "  Song  ",
            "Artist",
            "https://i.ytimg.com/vi/aqz-KE-bpKQ/hqdefault.jpg",
            215.0,
        )
        .unwrap()
    }

    #[test]
    fn identity_and_canonical_url_derive_from_one_video_id() {
        let media = sample();
        assert_eq!(media.id, "youtube:aqz-KE-bpKQ");
        assert_eq!(media.title, "Song");
        assert_eq!(media.video_id(), "aqz-KE-bpKQ");
        assert_eq!(
            media.source_url(),
            "https://www.youtube.com/watch?v=aqz-KE-bpKQ"
        );
        assert!(RemoteMedia::is_remote_id(&media.id));
        assert!(!RemoteMedia::is_remote_id("youtube:short"));
        assert!(!RemoteMedia::is_remote_id("aqz-KE-bpKQ"));
    }

    #[test]
    fn malformed_remote_media_is_rejected() {
        let valid = sample();
        let cases = [
            RemoteMedia {
                id: "youtube:aqz-KE-bp/Q".into(),
                ..valid.clone()
            },
            RemoteMedia {
                title: " ".into(),
                ..valid.clone()
            },
            RemoteMedia {
                title: "x".repeat(MAX_TITLE_CHARS + 1),
                ..valid.clone()
            },
            RemoteMedia {
                artist: "a\u{7}".into(),
                ..valid.clone()
            },
            RemoteMedia {
                artwork_url: "http://i.ytimg.com/a.jpg".into(),
                ..valid.clone()
            },
            RemoteMedia {
                artwork_url: "https://i.ytimg.com/a b.jpg".into(),
                ..valid.clone()
            },
            RemoteMedia {
                duration: f64::NAN,
                ..valid.clone()
            },
            RemoteMedia {
                duration: -1.0,
                ..valid.clone()
            },
        ];
        for case in cases {
            assert!(case.validate().is_err(), "accepted {case:?}");
        }
        assert!(
            RemoteMedia {
                artwork_url: String::new(),
                duration: 0.0,
                ..valid
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn upsert_keeps_known_metadata_when_newer_values_are_empty() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let media = sample();
        core.remember_remote(&media).unwrap();
        let sparse = RemoteMedia {
            title: "Renamed".into(),
            artist: String::new(),
            artwork_url: String::new(),
            duration: 0.0,
            ..media.clone()
        };
        core.remember_remote(&sparse).unwrap();
        let stored = core.remote_media(&media.id).unwrap().unwrap();
        assert_eq!(stored.title, "Renamed");
        assert_eq!(stored.artist, media.artist);
        assert_eq!(stored.artwork_url, media.artwork_url);
        assert_eq!(stored.duration, media.duration);
        assert!(core.remote_media("youtube:missing0000").unwrap().is_none());
    }
}
