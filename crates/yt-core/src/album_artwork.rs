//! Album covers found for online videos, remembered per video so the cover of
//! a song is looked up once. A lookup that found no cover is remembered too,
//! so a video without one is not asked about again on every play. The rows
//! reveal what was played, so they are erased with the listening history.

use crate::{
    AppCore, CoreError, Result,
    media::{valid_artwork_url, valid_key},
    now,
};
use rusqlite::{OptionalExtension, Row, params};

/// Videos remembered; the least recently checked are pruned on write.
pub const ALBUM_ARTWORK_LIMIT: usize = 2_000;

/// The remembered outcome of looking up one video's album cover.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumArtwork {
    /// Provider video id (11 URL-safe characters).
    pub video_id: String,
    /// `https` base URL of the cover; `None` when the video names none.
    pub url: Option<String>,
    /// When the provider was asked, in Unix seconds.
    pub checked_at: f64,
}

const COLUMNS: &str = "video_id,artwork_url,checked_at";

/// A row read back, or `None` when it no longer validates.
fn from_row(row: &Row<'_>) -> rusqlite::Result<Option<AlbumArtwork>> {
    let (video_id, url, checked_at): (String, String, f64) =
        (row.get(0)?, row.get(1)?, row.get(2)?);
    let valid = valid_key(&video_id) && valid_artwork_url(&url) && checked_at.is_finite();
    Ok(valid.then(|| AlbumArtwork {
        video_id,
        url: (!url.is_empty()).then_some(url),
        checked_at,
    }))
}

impl AppCore {
    /// Every remembered lookup, most recently checked first, at most
    /// `ALBUM_ARTWORK_LIMIT`. Rows that no longer validate are skipped.
    pub fn album_artworks(&self) -> Result<Vec<AlbumArtwork>> {
        let db = self.connection()?;
        let mut statement = db.prepare(&format!(
            "SELECT {COLUMNS} FROM rust_album_artwork ORDER BY checked_at DESC,video_id LIMIT ?1"
        ))?;
        let rows = statement.query_map([ALBUM_ARTWORK_LIMIT as i64], from_row)?;
        Ok(rows.filter_map(|row| row.ok().flatten()).collect())
    }

    /// The remembered lookup for `video_id`; a row that no longer validates
    /// reads as absent.
    pub fn album_artwork(&self, video_id: &str) -> Result<Option<AlbumArtwork>> {
        Ok(self
            .connection()?
            .query_row(
                &format!("SELECT {COLUMNS} FROM rust_album_artwork WHERE video_id=?1"),
                [video_id],
                from_row,
            )
            .optional()?
            .flatten())
    }

    /// Remember, as checked now, that `video_id`'s album cover is `url`, or
    /// that it has none (`None`). `url` must be an `https` URL within the
    /// artwork URL bounds. The table is then pruned to `ALBUM_ARTWORK_LIMIT`.
    pub fn store_album_artwork(&self, video_id: &str, url: Option<&str>) -> Result<()> {
        if !valid_key(video_id) {
            return Err(CoreError::Invalid("Invalid video identifier".into()));
        }
        if url.is_some_and(|url| url.is_empty() || !valid_artwork_url(url)) {
            return Err(CoreError::Invalid("Invalid album artwork URL".into()));
        }
        let mut db = self.connection()?;
        let transaction = db.transaction()?;
        transaction.execute(
            "INSERT INTO rust_album_artwork(video_id,artwork_url,checked_at) VALUES(?1,?2,?3)
             ON CONFLICT(video_id) DO UPDATE SET artwork_url=excluded.artwork_url,checked_at=excluded.checked_at",
            params![video_id, url.unwrap_or_default(), now()],
        )?;
        transaction.execute(
            "DELETE FROM rust_album_artwork WHERE video_id IN (
                SELECT video_id FROM rust_album_artwork ORDER BY checked_at DESC,video_id LIMIT -1 OFFSET ?1)",
            [ALBUM_ARTWORK_LIMIT as i64],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COVER: &str = "https://yt3.googleusercontent.com/cover";

    fn core() -> (tempfile::TempDir, AppCore) {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        (profile, core)
    }

    #[test]
    fn lookups_round_trip_and_the_latest_outcome_wins() {
        let (_profile, core) = core();
        core.store_album_artwork("abcdefghijk", Some(COVER))
            .unwrap();
        core.store_album_artwork("lmnopqrstuv", None).unwrap();
        let stored = core.album_artworks().unwrap();
        assert_eq!(stored.len(), 2);
        let found = stored.iter().find(|a| a.video_id == "abcdefghijk").unwrap();
        assert_eq!(found.url.as_deref(), Some(COVER));
        assert!(found.checked_at > 0.0);
        let missing = stored.iter().find(|a| a.video_id == "lmnopqrstuv").unwrap();
        assert_eq!(missing.url, None);
        assert_eq!(
            core.album_artwork("abcdefghijk").unwrap().as_ref(),
            Some(found)
        );
        assert_eq!(core.album_artwork("zzzzzzzzzzz").unwrap(), None);
        core.store_album_artwork("abcdefghijk", None).unwrap();
        let replaced = core.album_artworks().unwrap();
        assert!(replaced.iter().all(|artwork| artwork.url.is_none()));
    }

    #[test]
    fn invalid_input_is_rejected_and_corrupt_rows_are_skipped() {
        let (_profile, core) = core();
        assert!(core.store_album_artwork("short", Some(COVER)).is_err());
        assert!(core.store_album_artwork("abcdefghijk", Some("")).is_err());
        assert!(
            core.store_album_artwork("abcdefghijk", Some("http://insecure.test/a"))
                .is_err()
        );
        assert!(
            core.store_album_artwork("abcdefghijk", Some("https://a.test/a b"))
                .is_err()
        );
        core.connection()
            .unwrap()
            .execute_batch(
                "INSERT INTO rust_album_artwork VALUES('bad id','',1);
                 INSERT INTO rust_album_artwork VALUES('abcdefghijk','javascript:alert(1)',1);",
            )
            .unwrap();
        assert!(core.album_artworks().unwrap().is_empty());
        assert_eq!(core.album_artwork("abcdefghijk").unwrap(), None);
    }

    #[test]
    fn the_table_keeps_only_the_most_recently_checked_videos() {
        let (_profile, core) = core();
        {
            let mut db = core.connection().unwrap();
            let transaction = db.transaction().unwrap();
            for index in 0..ALBUM_ARTWORK_LIMIT {
                transaction
                    .execute(
                        "INSERT INTO rust_album_artwork VALUES(?1,'',?2)",
                        params![format!("{index:0>11}"), index as f64],
                    )
                    .unwrap();
            }
            transaction.commit().unwrap();
        }
        core.store_album_artwork("newestvideo", Some(COVER))
            .unwrap();
        let stored = core.album_artworks().unwrap();
        assert_eq!(stored.len(), ALBUM_ARTWORK_LIMIT);
        assert_eq!(stored[0].video_id, "newestvideo");
        assert!(
            !stored
                .iter()
                .any(|artwork| artwork.video_id == "00000000000")
        );
    }

    #[test]
    fn erasing_the_listening_history_forgets_every_lookup() {
        let (_profile, core) = core();
        core.store_album_artwork("abcdefghijk", Some(COVER))
            .unwrap();
        core.store_album_artwork("lmnopqrstuv", None).unwrap();
        let erased = core.clear_listening_history().unwrap();
        assert_eq!(erased.album_artwork, 2);
        assert!(core.album_artworks().unwrap().is_empty());
    }
}
