CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY, name TEXT NOT NULL,
    applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE TABLE IF NOT EXISTS tracks (
    id TEXT PRIMARY KEY,
    storage_kind TEXT NOT NULL DEFAULT 'managed' CHECK(storage_kind IN ('managed','reference')),
    relative_path TEXT, external_path TEXT,
    filename TEXT NOT NULL, title TEXT NOT NULL,
    artist TEXT NOT NULL DEFAULT '', album TEXT NOT NULL DEFAULT '',
    media_type TEXT NOT NULL CHECK(media_type IN ('audio','video')),
    mime_type TEXT NOT NULL DEFAULT '', extension TEXT NOT NULL DEFAULT '',
    size_bytes INTEGER NOT NULL DEFAULT 0 CHECK(size_bytes>=0),
    duration REAL NOT NULL DEFAULT 0 CHECK(duration>=0),
    bitrate INTEGER NOT NULL DEFAULT 0 CHECK(bitrate>=0),
    sample_rate INTEGER NOT NULL DEFAULT 0 CHECK(sample_rate>=0),
    source_url TEXT NOT NULL DEFAULT '', source_id TEXT NOT NULL DEFAULT '',
    artwork_path TEXT NOT NULL DEFAULT '', artwork_url TEXT NOT NULL DEFAULT '',
    legacy_playlist TEXT NOT NULL DEFAULT '', metadata_json TEXT NOT NULL DEFAULT '{}',
    missing INTEGER NOT NULL DEFAULT 0 CHECK(missing IN (0,1)),
    file_modified_at REAL NOT NULL DEFAULT 0, downloaded_at REAL NOT NULL DEFAULT 0,
    created_at REAL NOT NULL, updated_at REAL NOT NULL,
    CHECK((storage_kind='managed' AND relative_path IS NOT NULL AND external_path IS NULL)
       OR (storage_kind='reference' AND relative_path IS NULL AND external_path IS NOT NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_tracks_relative_path_unique ON tracks(relative_path COLLATE NOCASE) WHERE relative_path IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_tracks_external_path_unique ON tracks(external_path) WHERE external_path IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_tracks_library_listing ON tracks(missing,media_type,updated_at DESC);
CREATE TABLE IF NOT EXISTS playlists (
    id TEXT PRIMARY KEY, name TEXT NOT NULL COLLATE NOCASE UNIQUE,
    description TEXT NOT NULL DEFAULT '', artwork_path TEXT NOT NULL DEFAULT '',
    created_at REAL NOT NULL, updated_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS playlist_tracks (
    playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position>=0), added_at REAL NOT NULL,
    PRIMARY KEY(playlist_id,track_id), UNIQUE(playlist_id,position)
);
CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value_json TEXT NOT NULL,updated_at REAL NOT NULL);
CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY, kind TEXT NOT NULL, status TEXT NOT NULL,
    progress REAL NOT NULL DEFAULT 0 CHECK(progress>=0 AND progress<=100),
    message TEXT NOT NULL DEFAULT '', payload_json TEXT NOT NULL DEFAULT '{}',
    result_json TEXT NOT NULL DEFAULT '{}', error TEXT NOT NULL DEFAULT '',
    cancellable INTEGER NOT NULL DEFAULT 1 CHECK(cancellable IN (0,1)),
    plugin_id TEXT NOT NULL DEFAULT '', created_at REAL NOT NULL,
    started_at REAL, finished_at REAL, updated_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS plugin_states (
    plugin_id TEXT PRIMARY KEY, enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0,1)),
    installed_version TEXT NOT NULL DEFAULT '', config_json TEXT NOT NULL DEFAULT '{}',
    state_json TEXT NOT NULL DEFAULT '{}', permissions_json TEXT NOT NULL DEFAULT '[]',
    last_error TEXT NOT NULL DEFAULT '', installed_at REAL NOT NULL,updated_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS rust_schema_migrations(version INTEGER PRIMARY KEY,applied_at REAL NOT NULL);
CREATE TABLE IF NOT EXISTS rust_track_preferences (
    track_id TEXT PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    favorite INTEGER NOT NULL DEFAULT 0 CHECK(favorite IN (0,1))
);
CREATE TABLE IF NOT EXISTS rust_import_sources (
    source_path TEXT PRIMARY KEY,
    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    source_size INTEGER NOT NULL, source_modified REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS rust_library_exclusions (
    path TEXT PRIMARY KEY,
    removed_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS rust_playback_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    played_at REAL NOT NULL, position_seconds REAL NOT NULL DEFAULT 0 CHECK(position_seconds>=0)
);
CREATE INDEX IF NOT EXISTS idx_rust_history_recent ON rust_playback_history(played_at DESC);
INSERT OR IGNORE INTO schema_migrations(version,name) VALUES(1,'initial_schema'),(2,'indexes'),(3,'playlist_membership_invariants'),(4,'plugins_are_opt_in');
INSERT OR IGNORE INTO rust_schema_migrations(version,applied_at) VALUES(1,unixepoch());
INSERT OR IGNORE INTO rust_schema_migrations(version,applied_at) VALUES(2,unixepoch());
