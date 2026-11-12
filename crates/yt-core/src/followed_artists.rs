//! Artists the user follows, identified by their YouTube channel id.

use crate::{
    AppCore, CoreError, Result,
    media::{MAX_ARTIST_CHARS, MAX_ARTWORK_URL_BYTES},
    now,
};
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// Length of a YouTube channel id (`UC` and 22 characters).
const CHANNEL_ID_LENGTH: usize = 24;

/// A followed artist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FollowedArtist {
    /// YouTube channel id.
    pub id: String,
    pub name: String,
    /// `https` image URL, or empty.
    pub thumbnail: String,
    /// Unix seconds.
    pub followed_at: f64,
}

/// Whether `id` has the shape of a YouTube channel id.
pub fn is_channel_id(id: &str) -> bool {
    id.len() == CHANNEL_ID_LENGTH
        && id.starts_with("UC")
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

impl FollowedArtist {
    pub fn validate(&self) -> Result<()> {
        let name = self.name.trim();
        let thumbnail_ok = self.thumbnail.is_empty()
            || (self.thumbnail.starts_with("https://")
                && self.thumbnail.len() <= MAX_ARTWORK_URL_BYTES
                && !self
                    .thumbnail
                    .bytes()
                    .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control()));
        if !is_channel_id(&self.id)
            || name.is_empty()
            || name.chars().count() > MAX_ARTIST_CHARS
            || name.chars().any(char::is_control)
            || !thumbnail_ok
        {
            return Err(CoreError::Invalid("Invalid artist".into()));
        }
        Ok(())
    }
}

impl AppCore {
    /// Follow an artist; following again keeps the original date.
    pub fn follow_artist(&self, artist: &FollowedArtist) -> Result<()> {
        artist.validate()?;
        self.connection()?.execute(
            "INSERT INTO rust_followed_artists(artist_id,name,thumbnail,followed_at) VALUES(?1,?2,?3,?4)
             ON CONFLICT(artist_id) DO UPDATE SET name=excluded.name,
               thumbnail=CASE WHEN excluded.thumbnail<>'' THEN excluded.thumbnail ELSE rust_followed_artists.thumbnail END",
            params![artist.id, artist.name.trim(), artist.thumbnail, now()],
        )?;
        Ok(())
    }

    pub fn unfollow_artist(&self, artist_id: &str) -> Result<()> {
        self.connection()?.execute(
            "DELETE FROM rust_followed_artists WHERE artist_id=?1",
            [artist_id],
        )?;
        Ok(())
    }

    /// Followed artists, most recently followed first; corrupt rows skipped.
    pub fn followed_artists(&self) -> Result<Vec<FollowedArtist>> {
        let db = self.connection()?;
        let mut query = db.prepare(
            "SELECT artist_id,name,thumbnail,followed_at FROM rust_followed_artists ORDER BY followed_at DESC",
        )?;
        let rows = query.query_map([], |row| {
            Ok(FollowedArtist {
                id: row.get(0)?,
                name: row.get(1)?,
                thumbnail: row.get(2)?,
                followed_at: row.get(3)?,
            })
        })?;
        Ok(rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|artist| artist.validate().is_ok())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artist(id: &str) -> FollowedArtist {
        FollowedArtist {
            id: id.into(),
            name: "Artist".into(),
            thumbnail: String::new(),
            followed_at: 0.0,
        }
    }

    #[test]
    fn follows_round_trip_and_reject_bad_identities() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let id = "UCabcdefghijklmnopqrstuv";
        core.follow_artist(&artist(id)).unwrap();
        core.follow_artist(&artist(id)).unwrap();
        assert_eq!(core.followed_artists().unwrap().len(), 1);
        core.unfollow_artist(id).unwrap();
        assert!(core.followed_artists().unwrap().is_empty());
        for bad in [
            "UCshort",
            "XXabcdefghijklmnopqrstuv",
            "UCabcdefghijklmnopqrst/v",
        ] {
            assert!(core.follow_artist(&artist(bad)).is_err(), "{bad}");
        }
        let mut insecure = artist(id);
        insecure.thumbnail = "http://example.com/a.jpg".into();
        assert!(core.follow_artist(&insecure).is_err());
    }
}
