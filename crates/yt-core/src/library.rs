use crate::{
    AppCore, CoreError, HistoryEntry, ImportFailure, ImportMode, ImportReport, PlaybackState,
    Result, Track, now,
};
use lofty::{
    file::{AudioFile, TaggedFileExt},
    prelude::Accessor,
    probe::Probe,
};
use rusqlite::{OptionalExtension, params};
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

const TRACK_SELECT: &str = "SELECT t.*,COALESCE(p.favorite,0) AS favorite FROM tracks t
    LEFT JOIN rust_track_preferences p ON p.track_id=t.id";

pub fn supported_media(path: &Path) -> bool {
    path.extension().and_then(|x| x.to_str()).is_some_and(|x| {
        matches!(
            x.to_ascii_lowercase().as_str(),
            "mp3" | "mp4" | "m4a" | "webm" | "mkv" | "wav" | "flac" | "aac" | "ogg" | "opus"
        )
    })
}

fn clean_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches([' ', '.']);
    if cleaned.is_empty() {
        "media".into()
    } else {
        cleaned.chars().take(180).collect()
    }
}

fn media_kind(extension: &str) -> (&'static str, &'static str) {
    match extension {
        "mp4" => ("video", "video/mp4"),
        "webm" => ("video", "video/webm"),
        "mkv" => ("video", "video/x-matroska"),
        "mp3" => ("audio", "audio/mpeg"),
        "m4a" => ("audio", "audio/mp4"),
        "wav" => ("audio", "audio/wav"),
        "flac" => ("audio", "audio/flac"),
        "aac" => ("audio", "audio/aac"),
        "opus" | "ogg" => ("audio", "audio/ogg"),
        _ => ("audio", "application/octet-stream"),
    }
}

impl AppCore {
    /// Resolve a legacy/local cover without exposing paths outside the media
    /// directory or the application's artwork directory.
    pub fn artwork_path(&self, id: &str) -> Result<Option<PathBuf>> {
        let (descriptor, storage_kind): (String, String) = self
            .connection()?
            .query_row(
                "SELECT artwork_path,storage_kind FROM tracks WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound(id.into()))?;
        let track = self.track(id)?;
        let artwork_root = self.profile_dir().join("Artwork");
        let media_parent = track
            .path
            .canonicalize()
            .ok()
            .and_then(|path| path.parent().map(Path::to_owned));
        let valid = |path: PathBuf| -> Option<PathBuf> {
            let resolved = path.canonicalize().ok()?;
            let image = resolved
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| {
                    matches!(
                        e.to_ascii_lowercase().as_str(),
                        "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp"
                    )
                });
            let in_artwork = artwork_root
                .canonicalize()
                .ok()
                .is_some_and(|root| resolved.starts_with(root));
            let allowed = if storage_kind == "reference" {
                in_artwork || resolved.parent() == media_parent.as_deref()
            } else {
                in_artwork || resolved.starts_with(self.library_dir())
            };
            (image && allowed && resolved.is_file()).then_some(resolved)
        };
        if let Some((prefix, relative)) = descriptor.split_once(':') {
            let candidate = match prefix {
                "library" => Some(self.library_dir().join(relative)),
                "artwork" => Some(artwork_root.join(relative)),
                "external" if storage_kind == "reference" => Some(PathBuf::from(relative)),
                _ => None,
            };
            if let Some(path) = candidate.and_then(&valid) {
                return Ok(Some(path));
            }
        }
        if !track.path.as_os_str().is_empty() {
            for extension in ["jpg", "jpeg", "png", "webp", "gif", "bmp"] {
                let stem = track.path.file_stem().unwrap_or_default().to_string_lossy();
                let candidates = [
                    track.path.with_extension(extension),
                    artwork_root.join(format!("{stem}.{extension}")),
                    self.library_dir()
                        .join("thumbnails")
                        .join(format!("{stem}.{extension}")),
                ];
                for candidate in candidates {
                    if let Some(path) = valid(candidate) {
                        return Ok(Some(path));
                    }
                }
            }
        }
        Ok(None)
    }

    pub(crate) fn row_to_track(&self, row: &rusqlite::Row<'_>) -> rusqlite::Result<Track> {
        let kind: String = row.get("storage_kind")?;
        let path = if kind == "managed" {
            let relative = PathBuf::from(row.get::<_, String>("relative_path")?);
            // Legacy database values must not escape the managed library or
            // follow a symlink outside it when exposed to a media player.
            if relative.as_os_str().is_empty()
                || relative
                    .components()
                    .any(|c| !matches!(c, Component::Normal(_)))
            {
                PathBuf::new()
            } else {
                let candidate = self.library_dir().join(relative);
                match std::fs::canonicalize(&candidate) {
                    Ok(real) if real.starts_with(self.library_dir()) => real,
                    Ok(_) => PathBuf::new(),
                    Err(_) => candidate,
                }
            }
        } else {
            let path = PathBuf::from(row.get::<_, String>("external_path")?);
            if path.is_absolute() {
                path
            } else {
                PathBuf::new()
            }
        };
        let missing = path.as_os_str().is_empty() || !path.is_file();
        Ok(Track {
            id: row.get("id")?,
            title: row.get("title")?,
            artist: row.get("artist")?,
            album: row.get("album")?,
            filename: row.get("filename")?,
            path,
            media_type: row.get("media_type")?,
            extension: row.get("extension")?,
            duration: row.get("duration")?,
            size_bytes: row.get::<_, i64>("size_bytes")?.max(0) as u64,
            artwork_url: row.get("artwork_url")?,
            favorite: row.get("favorite")?,
            missing,
            created_at: row.get("created_at")?,
            source_url: row.get("source_url")?,
        })
    }

    pub fn library(&self, search: &str) -> Result<Vec<Track>> {
        let db = self.connection()?;
        let mut statement =
            db.prepare(&format!("{TRACK_SELECT} ORDER BY t.created_at DESC,t.id"))?;
        let search = search.trim().to_lowercase();
        // Rust Unicode lowercase keeps accented titles searchable rather than
        // relying on SQLite's ASCII-only LOWER/NOCASE behavior.
        let rows = statement.query_map([], |row| self.row_to_track(row))?;
        let mut tracks = Vec::new();
        for row in rows {
            let track = row?;
            if search.is_empty()
                || [&track.title, &track.artist, &track.album, &track.filename]
                    .iter()
                    .any(|s| s.to_lowercase().contains(&search))
            {
                tracks.push(track);
            }
        }
        Ok(tracks)
    }

    pub fn track(&self, id: &str) -> Result<Track> {
        self.connection()?
            .query_row(&format!("{TRACK_SELECT} WHERE t.id=?1"), [id], |row| {
                self.row_to_track(row)
            })
            .optional()?
            .ok_or_else(|| CoreError::NotFound(id.into()))
    }

    pub fn set_favorite(&self, id: &str, favorite: bool) -> Result<()> {
        self.connection()?.execute(
            "INSERT INTO rust_track_preferences(track_id,favorite) VALUES(?1,?2)
            ON CONFLICT(track_id) DO UPDATE SET favorite=excluded.favorite",
            params![id, favorite],
        )?;
        Ok(())
    }

    /// Remove the database entry and its playlist/history membership. The
    /// original file is intentionally never removed by this operation, and
    /// automatic scans keep it excluded until an explicit import restores it.
    pub fn remove_track(&self, id: &str) -> Result<()> {
        let _import = self.import_lock.lock().map_err(|_| CoreError::Lock)?;
        let path = self.track(id)?.path;
        let mut db = self.connection()?;
        let tx = db.transaction()?;
        let playlists: Vec<String> = {
            let mut query =
                tx.prepare("SELECT playlist_id FROM playlist_tracks WHERE track_id=?1")?;
            query
                .query_map([id], |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?
        };
        // Rebuild memberships instead of relying on the legacy compaction
        // trigger whose multi-row UPDATE can collide with unique positions.
        for playlist in playlists {
            let remaining = super::playlists::ordered_ids(&tx, &playlist)?
                .into_iter()
                .filter(|track| track != id)
                .collect::<Vec<_>>();
            super::playlists::replace_membership(&tx, &playlist, &remaining)?;
        }
        if tx.execute("DELETE FROM tracks WHERE id=?1", [id])? == 0 {
            return Err(CoreError::NotFound(id.into()));
        }
        if path.is_absolute() {
            tx.execute(
                "INSERT INTO rust_library_exclusions(path,removed_at) VALUES(?1,?2)
                 ON CONFLICT(path) DO UPDATE SET removed_at=excluded.removed_at",
                params![path.to_string_lossy(), now()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn import_file(&self, path: &Path, mode: ImportMode) -> Result<Track> {
        self.import_file_checked(path, mode, true)?
            .ok_or_else(|| CoreError::Invalid("Explicit import was unexpectedly excluded".into()))
    }

    fn import_file_checked(
        &self,
        path: &Path,
        mode: ImportMode,
        explicit: bool,
    ) -> Result<Option<Track>> {
        let _import = self.import_lock.lock().map_err(|_| CoreError::Lock)?;
        let source = std::fs::canonicalize(path)?;
        if !source.is_file() || !supported_media(&source) {
            return Err(CoreError::Invalid(format!(
                "Unsupported media file: {}",
                source.display()
            )));
        }
        let source_metadata = source.metadata()?;
        if source_metadata.len() == 0 {
            return Err(CoreError::Invalid("The media file is empty".into()));
        }
        if explicit {
            self.connection()?.execute(
                "DELETE FROM rust_library_exclusions WHERE path=?1",
                [source.to_string_lossy()],
            )?;
        } else {
            let excluded: bool = self.connection()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM rust_library_exclusions WHERE path=?1)",
                [source.to_string_lossy()],
                |row| row.get(0),
            )?;
            if excluded {
                return Ok(None);
            }
        }
        let modified = source_metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|t| t.as_secs_f64())
            .unwrap_or(0.0);
        let duplicate: Option<String> = self
            .connection()?
            .query_row(
                "SELECT s.track_id FROM rust_import_sources s JOIN tracks t ON t.id=s.track_id
             WHERE source_path=?1 AND source_size=?2 AND source_modified=?3",
                params![
                    source.to_string_lossy(),
                    source_metadata.len() as i64,
                    modified
                ],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = duplicate
            && let Ok(track) = self.track(&id)
            && !track.missing
        {
            return Ok(Some(track));
        }
        let existing: Option<String> = {
            let relative = source
                .strip_prefix(self.library_dir())
                .ok()
                .map(|p| p.to_string_lossy().to_string());
            self.connection()?
                .query_row(
                    "SELECT id FROM tracks WHERE external_path=?1 OR relative_path=?2",
                    params![source.to_string_lossy(), relative],
                    |r| r.get(0),
                )
                .optional()?
        };
        if let Some(id) = existing {
            return self.track(&id).map(Some);
        }

        let id = uuid::Uuid::new_v4().to_string();
        let filename = clean_filename(&source.file_name().unwrap_or_default().to_string_lossy());
        let extension = source
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        let already_managed = source.starts_with(self.library_dir());
        let destination = if mode == ImportMode::Copy && !already_managed {
            let destination = self.library_dir().join(format!("{id}-{filename}"));
            let mut input = File::open(&source)?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)?;
            if let Err(error) =
                std::io::copy(&mut input, &mut output).and_then(|_| output.sync_all())
            {
                let _ = std::fs::remove_file(&destination);
                return Err(error.into());
            }
            destination
        } else {
            source.clone()
        };
        let copied = destination != source;
        let result = self.insert_media(&destination, &filename, &extension, &id);
        let track = match result {
            Ok(track) => track,
            Err(error) => {
                if copied {
                    let _ = std::fs::remove_file(destination);
                }
                return Err(error);
            }
        };
        self.connection()?.execute(
            "INSERT INTO rust_import_sources(source_path,track_id,source_size,source_modified)
            VALUES(?1,?2,?3,?4) ON CONFLICT(source_path) DO UPDATE SET track_id=excluded.track_id,
            source_size=excluded.source_size,source_modified=excluded.source_modified",
            params![
                source.to_string_lossy(),
                track.id,
                source_metadata.len() as i64,
                modified
            ],
        )?;
        Ok(Some(track))
    }

    fn insert_media(
        &self,
        destination: &Path,
        filename: &str,
        extension: &str,
        id: &str,
    ) -> Result<Track> {
        let mut title = Path::new(filename)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut artist = String::new();
        let mut album = String::new();
        let mut duration = 0.0;
        let mut bitrate = 0u32;
        let mut sample_rate = 0u32;
        if let Ok(tagged) = Probe::open(destination)
            .and_then(|p| Ok(p.guess_file_type()?))
            .and_then(|p| p.read())
        {
            duration = tagged.properties().duration().as_secs_f64();
            bitrate = tagged.properties().audio_bitrate().unwrap_or(0);
            sample_rate = tagged.properties().sample_rate().unwrap_or(0);
            if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
                if let Some(value) = tag.title().filter(|v| !v.trim().is_empty()) {
                    title = value.into_owned();
                }
                artist = tag.artist().map(|v| v.into_owned()).unwrap_or_default();
                album = tag.album().map(|v| v.into_owned()).unwrap_or_default();
            }
        }
        let (media_type, mime_type) = media_kind(extension);
        let metadata = destination.metadata()?;
        let timestamp = now();
        let relative = destination
            .strip_prefix(self.library_dir())
            .ok()
            .map(|p| p.to_string_lossy().to_string());
        let storage_kind = if relative.is_some() {
            "managed"
        } else {
            "reference"
        };
        let external = relative
            .is_none()
            .then(|| destination.to_string_lossy().to_string());
        self.connection()?.execute("INSERT INTO tracks(id,storage_kind,relative_path,external_path,filename,title,artist,album,
            media_type,mime_type,extension,size_bytes,duration,bitrate,sample_rate,file_modified_at,created_at,updated_at)
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?16,?16)",
            params![id,storage_kind,relative,external,filename,title,artist,album,media_type,mime_type,extension,
                metadata.len() as i64,duration,bitrate,sample_rate,timestamp])?;
        self.track(id)
    }

    pub fn import_bytes(&self, filename: &str, bytes: &[u8]) -> Result<Track> {
        let filename = clean_filename(filename);
        if !supported_media(Path::new(&filename)) || bytes.is_empty() {
            return Err(CoreError::Invalid(
                "Select a supported, non-empty media file".into(),
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let path = self.library_dir().join(format!("{id}-{filename}"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let result = file
            .write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(CoreError::from)
            .and_then(|_| {
                let extension = Path::new(&filename)
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_ascii_lowercase();
                self.insert_media(&path, &filename, &extension, &id)
            });
        if result.is_err() {
            let _ = std::fs::remove_file(path);
        }
        result
    }

    /// Import directories without following directory symlinks. M3U entries
    /// resolve relative to the playlist, and unsupported remote entries are
    /// reported rather than fetched implicitly.
    pub fn import_paths(&self, paths: &[PathBuf], mode: ImportMode) -> Result<ImportReport> {
        self.import_paths_checked(paths, mode, true)
    }

    fn import_paths_checked(
        &self,
        paths: &[PathBuf],
        mode: ImportMode,
        explicit: bool,
    ) -> Result<ImportReport> {
        let mut pending = paths.to_vec();
        let mut visited = HashSet::new();
        let mut track_ids = HashSet::new();
        let mut source_tracks = HashMap::new();
        let mut requested_playlists = Vec::new();
        let mut report = ImportReport::default();
        while let Some(path) = pending.pop() {
            if visited.len() >= 50_000 {
                return Err(CoreError::Invalid(
                    "Import contains more than 50,000 paths".into(),
                ));
            }
            let canonical = match std::fs::canonicalize(&path) {
                Ok(path) => path,
                Err(error) => {
                    report.errors.push(ImportFailure {
                        path,
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            if !visited.insert(canonical.clone()) {
                continue;
            }
            if canonical.is_dir() {
                match std::fs::read_dir(&canonical) {
                    Ok(entries) => {
                        for entry in entries {
                            match entry {
                                Ok(entry)
                                    if entry.file_type().is_ok_and(|kind| !kind.is_symlink()) =>
                                {
                                    pending.push(entry.path())
                                }
                                Ok(_) => {}
                                Err(error) => report.errors.push(ImportFailure {
                                    path: canonical.clone(),
                                    message: error.to_string(),
                                }),
                            }
                        }
                    }
                    Err(error) => report.errors.push(ImportFailure {
                        path: canonical,
                        message: error.to_string(),
                    }),
                }
            } else if matches!(
                canonical
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_lowercase)
                    .as_deref(),
                Some("m3u" | "m3u8")
            ) {
                if std::fs::metadata(&canonical)?.len() > 8 * 1024 * 1024 {
                    report.errors.push(ImportFailure {
                        path: canonical,
                        message: "Playlist exceeds the 8 MiB import limit".into(),
                    });
                    continue;
                }
                match std::fs::read_to_string(&canonical) {
                    Ok(contents) => {
                        let mut entries = Vec::new();
                        for line in contents.lines() {
                            let line = line.trim().trim_start_matches('\u{feff}');
                            if line.is_empty() || line.starts_with('#') {
                                continue;
                            }
                            if line.contains("://") {
                                report.errors.push(ImportFailure{path:canonical.clone(),message:"Remote playlist entries must be downloaded before importing".into()});
                                continue;
                            }
                            let entry = PathBuf::from(line);
                            let entry = if entry.is_absolute() {
                                entry
                            } else {
                                canonical.parent().unwrap_or(Path::new(".")).join(entry)
                            };
                            entries.push(entry.clone());
                            pending.push(entry);
                        }
                        let name = canonical
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .unwrap_or("Playlist")
                            .to_owned();
                        requested_playlists.push((canonical.clone(), name, entries));
                    }
                    Err(error) => report.errors.push(ImportFailure {
                        path: canonical,
                        message: error.to_string(),
                    }),
                }
            } else if supported_media(&canonical) {
                match self.import_file_checked(&canonical, mode, explicit) {
                    Ok(Some(track)) => {
                        source_tracks.insert(canonical, track.id.clone());
                        if track_ids.insert(track.id.clone()) {
                            report.tracks.push(track);
                        }
                    }
                    Ok(None) => {}
                    Err(error) => report.errors.push(ImportFailure {
                        path: canonical,
                        message: error.to_string(),
                    }),
                }
            } else if paths.contains(&path) {
                report.errors.push(ImportFailure {
                    path: canonical,
                    message: "Unsupported file format".into(),
                });
            }
        }
        for (path, name, entries) in requested_playlists {
            let ids = entries
                .into_iter()
                .filter_map(|entry| std::fs::canonicalize(entry).ok())
                .filter_map(|entry| source_tracks.get(&entry).cloned())
                .collect::<Vec<_>>();
            if ids.is_empty() {
                continue;
            }
            let result = (|| -> Result<crate::Playlist> {
                let existing_id: Option<String> = self
                    .connection()?
                    .query_row(
                        "SELECT id FROM playlists WHERE name=?1 COLLATE NOCASE",
                        [&name],
                        |row| row.get(0),
                    )
                    .optional()?;
                let playlist = match self
                    .playlists()?
                    .into_iter()
                    .find(|playlist| Some(&playlist.id) == existing_id.as_ref())
                {
                    Some(playlist) => playlist,
                    None => self.create_playlist(&name)?,
                };
                for id in ids {
                    self.add_to_playlist(&playlist.id, &id)?;
                }
                Ok(crate::Playlist {
                    track_count: self.playlist_tracks(&playlist.id)?.len() as u64,
                    ..playlist
                })
            })();
            match result {
                Ok(playlist) => report.playlists.push(playlist),
                Err(error) => report.errors.push(ImportFailure {
                    path,
                    message: error.to_string(),
                }),
            }
        }
        Ok(report)
    }

    pub fn scan_library(&self) -> Result<ImportReport> {
        self.import_paths_checked(
            &[self.library_dir().to_owned()],
            ImportMode::Reference,
            false,
        )
    }

    pub fn update_track_metadata(
        &self,
        id: &str,
        title: &str,
        artist: &str,
        artwork_url: &str,
        source_url: &str,
    ) -> Result<Track> {
        if title.trim().is_empty() || title.len() > 2000 {
            return Err(CoreError::Invalid("Invalid title".into()));
        }
        if self.connection()?.execute("UPDATE tracks SET title=?1,artist=?2,artwork_url=?3,source_url=?4,updated_at=?5 WHERE id=?6",
            params![title.trim(),artist,artwork_url,source_url,now(),id])? == 0 { return Err(CoreError::NotFound(id.into())); }
        self.track(id)
    }

    pub fn save_playback(&self, state: &PlaybackState) -> Result<()> {
        if !state.position_seconds.is_finite()
            || state.position_seconds < 0.0
            || !state.volume.is_finite()
            || !(0.0..=1.0).contains(&state.volume)
            || !state.playback_rate.is_finite()
            || !(0.25..=4.0).contains(&state.playback_rate)
            || state.current_index.is_some_and(|i| i >= state.queue.len())
            || !["off", "all", "one"].contains(&state.repeat.as_str())
        {
            return Err(CoreError::Invalid("Invalid playback state".into()));
        }
        self.set_setting("rust.playback", &serde_json::to_value(state)?)
    }

    pub fn load_playback(&self) -> Result<PlaybackState> {
        match self.setting("rust.playback")? {
            Some(value) => Ok(serde_json::from_value(value)?),
            None => Ok(PlaybackState {
                volume: 1.0,
                playback_rate: 1.0,
                repeat: "off".into(),
                ..Default::default()
            }),
        }
    }

    pub fn record_playback(&self, id: &str, position_seconds: f64) -> Result<()> {
        if !position_seconds.is_finite() || position_seconds < 0.0 {
            return Err(CoreError::Invalid("Invalid playback position".into()));
        }
        let mut db = self.connection()?;
        let tx = db.transaction()?;
        tx.execute("INSERT INTO rust_playback_history(track_id,played_at,position_seconds) VALUES(?1,?2,?3)",params![id,now(),position_seconds])?;
        tx.execute("DELETE FROM rust_playback_history WHERE id NOT IN (SELECT id FROM rust_playback_history ORDER BY played_at DESC,id DESC LIMIT 500)",[])?;
        tx.commit()?;
        Ok(())
    }

    pub fn history(&self, limit: usize) -> Result<Vec<HistoryEntry>> {
        let db = self.connection()?;
        let mut query = db.prepare("SELECT t.*,COALESCE(p.favorite,0) AS favorite,h.played_at,h.position_seconds
            FROM rust_playback_history h JOIN tracks t ON t.id=h.track_id
            LEFT JOIN rust_track_preferences p ON p.track_id=t.id ORDER BY h.played_at DESC,h.id DESC LIMIT ?1")?;
        let rows = query.query_map([limit.min(500) as i64], |row| {
            Ok(HistoryEntry {
                track: self.row_to_track(row)?,
                played_at: row.get("played_at")?,
                position_seconds: row.get("position_seconds")?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}
