use crate::{AppCore, CoreError, Playlist, Result, Track, now};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashSet;

fn validate_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > 240
        || name.chars().any(|c| c.is_control() || "/\\".contains(c))
    {
        Err(CoreError::Invalid(
            "Playlist names must contain 1–240 characters without slashes or control characters"
                .into(),
        ))
    } else {
        Ok(name)
    }
}

pub(crate) fn ordered_ids(db: &Connection, id: &str) -> Result<Vec<String>> {
    let mut query =
        db.prepare("SELECT track_id FROM playlist_tracks WHERE playlist_id=?1 ORDER BY position")?;
    Ok(query
        .query_map([id], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?)
}

pub(crate) fn replace_membership(db: &Connection, id: &str, tracks: &[String]) -> Result<()> {
    let existing = ordered_ids(db, id)?;
    // Legacy databases contain a position-compaction trigger. Deleting last
    // first prevents unique-position conflicts in that trigger's UPDATE.
    for track in existing.iter().rev() {
        db.execute(
            "DELETE FROM playlist_tracks WHERE playlist_id=?1 AND track_id=?2",
            params![id, track],
        )?;
    }
    for (index, track) in tracks.iter().enumerate() {
        db.execute("INSERT INTO playlist_tracks(playlist_id,track_id,position,added_at) VALUES(?1,?2,?3,?4)",params![id,track,index as i64,now()])?;
    }
    db.execute(
        "UPDATE playlists SET updated_at=?1 WHERE id=?2",
        params![now(), id],
    )?;
    Ok(())
}

impl AppCore {
    pub fn playlists(&self) -> Result<Vec<Playlist>> {
        let db = self.connection()?;
        let mut query=db.prepare("SELECT p.id,p.name,p.description,COUNT(pt.track_id) AS track_count FROM playlists p
            LEFT JOIN playlist_tracks pt ON pt.playlist_id=p.id GROUP BY p.id ORDER BY p.name COLLATE NOCASE,p.id")?;
        let rows = query.query_map([], |r| {
            Ok(Playlist {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                track_count: r.get::<_, i64>(3)? as u64,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn playlist_tracks(&self, id: &str) -> Result<Vec<Track>> {
        let db = self.connection()?;
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM playlists WHERE id=?1)",
            [id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(CoreError::NotFound(id.into()));
        }
        let mut query=db.prepare("SELECT t.*,COALESCE(p.favorite,0) AS favorite FROM playlist_tracks pt JOIN tracks t ON t.id=pt.track_id
            LEFT JOIN rust_track_preferences p ON p.track_id=t.id WHERE pt.playlist_id=?1 ORDER BY pt.position")?;
        let rows = query.query_map([id], |row| self.row_to_track(row))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn create_playlist(&self, name: &str) -> Result<Playlist> {
        let name = validate_name(name)?;
        let playlist = Playlist {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            description: String::new(),
            track_count: 0,
        };
        self.connection()?.execute(
            "INSERT INTO playlists(id,name,created_at,updated_at) VALUES(?1,?2,?3,?3)",
            params![playlist.id, name, now()],
        )?;
        Ok(playlist)
    }

    pub fn rename_playlist(&self, id: &str, name: &str) -> Result<()> {
        let name = validate_name(name)?;
        if self.connection()?.execute(
            "UPDATE playlists SET name=?1,updated_at=?2 WHERE id=?3",
            params![name, now(), id],
        )? == 0
        {
            return Err(CoreError::NotFound(id.into()));
        }
        Ok(())
    }

    pub fn delete_playlist(&self, id: &str) -> Result<()> {
        let mut db = self.connection()?;
        let tx = db.transaction()?;
        replace_membership(&tx, id, &[])?;
        if tx.execute("DELETE FROM playlists WHERE id=?1", [id])? == 0 {
            return Err(CoreError::NotFound(id.into()));
        }
        tx.commit()?;
        Ok(())
    }

    pub fn add_to_playlist(&self, playlist_id: &str, track_id: &str) -> Result<()> {
        let mut db = self.connection()?;
        let tx = db.transaction()?;
        let exists: Option<i64> = tx
            .query_row(
                "SELECT position FROM playlist_tracks WHERE playlist_id=?1 AND track_id=?2",
                params![playlist_id, track_id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            tx.execute("INSERT INTO playlist_tracks(playlist_id,track_id,position,added_at)
                VALUES(?1,?2,(SELECT COALESCE(MAX(position),-1)+1 FROM playlist_tracks WHERE playlist_id=?1),?3)",params![playlist_id,track_id,now()])?;
            tx.execute(
                "UPDATE playlists SET updated_at=?1 WHERE id=?2",
                params![now(), playlist_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_from_playlist(&self, playlist_id: &str, track_id: &str) -> Result<()> {
        let mut db = self.connection()?;
        let tx = db.transaction()?;
        let tracks = ordered_ids(&tx, playlist_id)?;
        if !tracks.iter().any(|id| id == track_id) {
            return Err(CoreError::NotFound(track_id.into()));
        }
        let remaining = tracks
            .into_iter()
            .filter(|id| id != track_id)
            .collect::<Vec<_>>();
        replace_membership(&tx, playlist_id, &remaining)?;
        tx.commit()?;
        Ok(())
    }

    /// Reorder requires a complete permutation. Invalid input changes nothing.
    pub fn reorder_playlist(&self, id: &str, track_ids: &[String]) -> Result<()> {
        let mut db = self.connection()?;
        let tx = db.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM playlists WHERE id=?1)",
            [id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(CoreError::NotFound(id.into()));
        }
        let current = ordered_ids(&tx, id)?;
        let expected: HashSet<_> = current.iter().collect();
        let requested: HashSet<_> = track_ids.iter().collect();
        if current.len() != track_ids.len()
            || requested.len() != track_ids.len()
            || expected != requested
        {
            return Err(CoreError::Invalid(
                "Playlist order must contain every current track exactly once".into(),
            ));
        }
        replace_membership(&tx, id, track_ids)?;
        tx.commit()?;
        Ok(())
    }
}
