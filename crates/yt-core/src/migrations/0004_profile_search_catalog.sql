CREATE TABLE rust_audio_features (
    media_id TEXT PRIMARY KEY,
    analyzed_seconds REAL NOT NULL CHECK(analyzed_seconds>=0),
    energy REAL NOT NULL CHECK(energy BETWEEN 0 AND 1),
    dynamics REAL NOT NULL CHECK(dynamics BETWEEN 0 AND 1),
    brightness REAL NOT NULL CHECK(brightness BETWEEN 0 AND 1),
    onset_rate REAL NOT NULL CHECK(onset_rate BETWEEN 0 AND 1),
    tempo_bpm REAL NOT NULL CHECK(tempo_bpm>=0),
    mode REAL NOT NULL CHECK(mode BETWEEN -1 AND 1),
    updated_at REAL NOT NULL
);
CREATE TABLE rust_search_history (
    query_key TEXT PRIMARY KEY,
    query TEXT NOT NULL,
    uses INTEGER NOT NULL CHECK(uses>=1),
    first_at REAL NOT NULL,
    last_at REAL NOT NULL
);
CREATE INDEX idx_rust_search_recent ON rust_search_history(last_at DESC);
CREATE TABLE rust_followed_artists (
    artist_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    thumbnail TEXT NOT NULL DEFAULT '',
    followed_at REAL NOT NULL
);
CREATE TABLE rust_provider_cache (
    key TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL,
    fetched_at REAL NOT NULL
);
DROP TABLE rust_discovery_cache;
