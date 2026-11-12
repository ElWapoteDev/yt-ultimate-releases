CREATE TABLE rust_remote_media (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    artist TEXT NOT NULL DEFAULT '',
    artwork_url TEXT NOT NULL DEFAULT '',
    duration REAL NOT NULL DEFAULT 0 CHECK(duration>=0),
    first_seen_at REAL NOT NULL,
    updated_at REAL NOT NULL
);
CREATE TABLE rust_favorites (
    media_id TEXT PRIMARY KEY,
    added_at REAL NOT NULL
);
CREATE INDEX idx_rust_favorites_recent ON rust_favorites(added_at DESC);
CREATE TABLE rust_listening_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    media_id TEXT NOT NULL,
    source TEXT NOT NULL CHECK(source IN ('user','autoplay')),
    started_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    listened_seconds REAL NOT NULL DEFAULT 0 CHECK(listened_seconds>=0),
    duration REAL NOT NULL DEFAULT 0 CHECK(duration>=0),
    end_reason TEXT NOT NULL CHECK(end_reason IN ('playing','finished','interrupted','unknown'))
);
CREATE INDEX idx_rust_listening_recent ON rust_listening_events(started_at DESC, id DESC);
CREATE INDEX idx_rust_listening_media ON rust_listening_events(media_id);
CREATE TABLE rust_discovery_cache (
    key TEXT PRIMARY KEY,
    items_json TEXT NOT NULL,
    fetched_at REAL NOT NULL
);
INSERT OR IGNORE INTO rust_favorites(media_id, added_at)
    SELECT p.track_id, t.updated_at FROM rust_track_preferences p JOIN tracks t ON t.id = p.track_id
    WHERE p.favorite = 1;
INSERT INTO rust_listening_events(media_id, source, started_at, updated_at, listened_seconds, duration, end_reason)
    SELECT h.track_id, 'user', h.played_at, h.played_at, 0, t.duration, 'unknown'
    FROM rust_playback_history h JOIN tracks t ON t.id = h.track_id
    ORDER BY h.played_at, h.id;
DROP TABLE rust_track_preferences;
DROP TABLE rust_playback_history;
