CREATE TABLE rust_album_artwork (
    video_id TEXT PRIMARY KEY,
    artwork_url TEXT NOT NULL DEFAULT '',
    checked_at REAL NOT NULL
);
CREATE INDEX idx_rust_album_artwork_checked ON rust_album_artwork(checked_at DESC);
