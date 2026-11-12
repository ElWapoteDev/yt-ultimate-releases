//! Favorites of library tracks and remembered online media, in one table.

use crate::{
    AppCore, CoreError, MediaItem, RemoteMedia, Result, Track,
    library::{FAVORITE_JOIN, TRACK_COLUMNS},
    media::{media_exists, prune_orphan_remote_media, remote_rows, upsert_remote},
    now,
};
use rusqlite::{Connection, params};
use std::collections::HashSet;

/// A favorite and when the user added it.
#[derive(Debug, Clone, PartialEq)]
pub struct FavoriteEntry {
    pub item: MediaItem,
    /// Unix seconds.
    pub added_at: f64,
}

/// Adding keeps the original date, so re-adding never reorders favorites.
fn write_favorite(db: &Connection, id: &str, favorite: bool) -> Result<()> {
    if favorite {
        db.execute(
            "INSERT INTO rust_favorites(media_id,added_at) VALUES(?1,?2)
             ON CONFLICT(media_id) DO NOTHING",
            params![id, now()],
        )?;
    } else {
        db.execute("DELETE FROM rust_favorites WHERE media_id=?1", [id])?;
    }
    Ok(())
}

impl AppCore {
    /// Mark or unmark a library track or remembered online media. Unknown
    /// identifiers are rejected; use `set_remote_favorite` for media that may
    /// not be remembered yet.
    pub fn set_favorite(&self, id: &str, favorite: bool) -> Result<()> {
        let mut db = self.connection()?;
        let transaction = db.transaction()?;
        if !media_exists(&transaction, id)? {
            return Err(CoreError::NotFound(id.into()));
        }
        write_favorite(&transaction, id, favorite)?;
        if !favorite {
            prune_orphan_remote_media(&transaction)?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Remember online media and set its favorite flag in one transaction.
    pub fn set_remote_favorite(&self, media: &RemoteMedia, favorite: bool) -> Result<()> {
        let mut db = self.connection()?;
        let transaction = db.transaction()?;
        upsert_remote(&transaction, media)?;
        write_favorite(&transaction, &media.id, favorite)?;
        if !favorite {
            prune_orphan_remote_media(&transaction)?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Identifiers of every favorite, for marking items anywhere in the
    /// interface without loading them.
    pub fn favorite_ids(&self) -> Result<HashSet<String>> {
        let db = self.connection()?;
        let mut query = db.prepare("SELECT media_id FROM rust_favorites")?;
        Ok(query
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// Every favorite, newest first. Library tracks whose file is missing are
    /// included with `missing` set so the interface can explain them.
    pub fn favorites(&self) -> Result<Vec<FavoriteEntry>> {
        let db = self.connection()?;
        let mut entries = Vec::new();
        let mut local = db.prepare(&format!(
            "SELECT {TRACK_COLUMNS},fav.added_at AS favorite_added_at FROM rust_favorites fav
             JOIN tracks t ON t.id=fav.media_id {FAVORITE_JOIN}"
        ))?;
        let rows = local.query_map([], |row| {
            Ok((self.row_to_track(row)?, row.get("favorite_added_at")?))
        })?;
        for row in rows {
            let (track, added_at): (Track, f64) = row?;
            entries.push(FavoriteEntry {
                item: MediaItem::Local(track),
                added_at,
            });
        }
        let mut remote = db.prepare(
            "SELECT r.id,r.title,r.artist,r.artwork_url,r.duration,fav.added_at
             FROM rust_favorites fav JOIN rust_remote_media r ON r.id=fav.media_id",
        )?;
        for (media, added_at) in remote_rows(&mut remote, [])? {
            entries.push(FavoriteEntry {
                item: MediaItem::Remote(media),
                added_at,
            });
        }
        entries.sort_by(|left, right| {
            right
                .added_at
                .total_cmp(&left.added_at)
                .then_with(|| left.item.id().cmp(right.item.id()))
        });
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(video: &str) -> RemoteMedia {
        RemoteMedia::from_parts(video, "Remote song", "Channel", "", 200.0).unwrap()
    }

    #[test]
    fn local_and_remote_favorites_share_one_ordered_list() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let track = core.import_bytes("local.wav", &[1, 2, 3, 4]).unwrap();
        let online = remote("aqz-KE-bpKQ");
        core.set_favorite(&track.id, true).unwrap();
        core.set_remote_favorite(&online, true).unwrap();
        assert!(core.track(&track.id).unwrap().favorite);
        let ids = core.favorite_ids().unwrap();
        assert!(ids.contains(&track.id) && ids.contains(&online.id));
        let favorites = core.favorites().unwrap();
        assert_eq!(favorites.len(), 2);
        assert!(favorites[0].added_at >= favorites[1].added_at);
        let added_at = favorites
            .iter()
            .find(|entry| entry.item.id() == track.id)
            .unwrap()
            .added_at;
        core.set_favorite(&track.id, true).unwrap();
        assert_eq!(
            core.favorites()
                .unwrap()
                .iter()
                .find(|entry| entry.item.id() == track.id)
                .unwrap()
                .added_at,
            added_at
        );
    }

    #[test]
    fn unknown_ids_are_rejected_and_unfavorited_remote_media_is_forgotten() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        assert!(matches!(
            core.set_favorite("missing", true),
            Err(CoreError::NotFound(_))
        ));
        let online = remote("abcdefghijk");
        core.set_remote_favorite(&online, true).unwrap();
        assert!(core.remote_media(&online.id).unwrap().is_some());
        core.set_favorite(&online.id, false).unwrap();
        assert!(core.favorites().unwrap().is_empty());
        assert!(core.remote_media(&online.id).unwrap().is_none());
    }

    #[test]
    fn removing_a_track_removes_its_favorite() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let track = core.import_bytes("gone.wav", &[1, 2, 3, 4]).unwrap();
        core.set_favorite(&track.id, true).unwrap();
        core.remove_track(&track.id).unwrap();
        assert!(core.favorite_ids().unwrap().is_empty());
    }
}
