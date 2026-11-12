CREATE TABLE rust_dj_analysis (
    media_id TEXT NOT NULL,
    content_key TEXT NOT NULL,
    analyzer_version INTEGER NOT NULL,
    analysis TEXT NOT NULL,
    used_at REAL NOT NULL,
    PRIMARY KEY (media_id, content_key, analyzer_version)
);
CREATE INDEX idx_rust_dj_analysis_used ON rust_dj_analysis(used_at);
