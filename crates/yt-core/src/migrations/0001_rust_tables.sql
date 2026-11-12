CREATE TABLE IF NOT EXISTS rust_track_preferences (
    track_id TEXT PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    favorite INTEGER NOT NULL DEFAULT 0 CHECK(favorite IN (0,1))
);
CREATE TABLE IF NOT EXISTS rust_import_sources (
    source_path TEXT PRIMARY KEY,
    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    source_size INTEGER NOT NULL, source_modified REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS rust_playback_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    played_at REAL NOT NULL, position_seconds REAL NOT NULL DEFAULT 0 CHECK(position_seconds>=0)
);
CREATE INDEX IF NOT EXISTS idx_rust_history_recent ON rust_playback_history(played_at DESC);
