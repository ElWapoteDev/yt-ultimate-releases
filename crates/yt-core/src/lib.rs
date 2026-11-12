//! In-process application state for the Windows and macOS Rust frontends.
//! No Python runtime, HTTP service or child process is required.

mod album_artwork;
mod artwork;
mod audio_features;
mod cleanup;
mod credits;
mod discovery_cache;
mod dj_analysis;
mod favorites;
mod followed_artists;
mod jobs;
mod library;
mod library_index;
mod listening;
mod lyrics;
mod media;
mod migrations;
mod models;
mod normalize;
mod playlists;
mod search_history;
mod settings;
mod source_rights;
mod track_analysis;
mod video_metadata;

pub use album_artwork::{ALBUM_ARTWORK_LIMIT, AlbumArtwork};
pub use artwork::{MAX_ARTWORK_BYTES, artwork_samples, is_video_frame, validate_artwork};
pub use audio_features::AudioFeatures;
pub use credits::{CreditRole, Credits, MAX_CREDIT_CHARS, MAX_CREDIT_NAMES, MAX_CREDIT_ROLES};
pub use discovery_cache::{
    CachedDiscovery, CachedPayload, DISCOVERY_CACHE_LIMIT, DISCOVERY_LIST_LIMIT, MAX_PAYLOAD_BYTES,
};
pub use dj_analysis::{DJ_ANALYSIS_LIMIT, MAX_ANALYSIS_KEY_BYTES};
pub use favorites::FavoriteEntry;
pub use followed_artists::{FollowedArtist, is_channel_id};
pub use library_index::{
    LibraryAlbum, LibraryArtist, library_album, library_albums, library_artists,
};
pub use listening::{
    LISTENING_EVENT_LIMIT, ListenEnd, ListenId, ListenProgress, ListenSignal, ListenSource,
    ListenTarget, ListeningErasure,
};
pub use lyrics::{
    CachedLyrics, LyricLine, LyricWord, Lyrics, LyricsCandidate, LyricsLookup, LyricsQuery,
    LyricsSource, MAX_LINE_CHARS, MAX_LINE_WORDS, MAX_LYRICS_BYTES, MAX_LYRICS_LINES,
};
pub use media::{MediaItem, REMOTE_ID_PREFIX, RemoteMedia};
pub use models::*;
pub use normalize::{
    FOLDED_APOSTROPHE, SongTitle, artist_key, clean_artist, contains_words, fold, fold_into,
    fold_words, song_title, title_key,
};
pub use search_history::{MAX_SEARCH_CHARS, SEARCH_HISTORY_LIMIT, SearchHistoryEntry};
pub use settings::{
    AppearancePreferences, CrossfadeCurve, DiscoveryPreferences, DjStyle, LyricsPreferences,
    MixLength, PlaybackPreferences, RegionPreferences, Settings,
};
pub use source_rights::{MediaOrigin, SourceRights};
pub use track_analysis::{
    BandEnergy, KeyEstimate, KeyMode, Meter, PitchClass, Section, Subdivision, TrackAnalysis,
};

use rusqlite::{Connection, OptionalExtension};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("Database: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("File: {0}")]
    Io(#[from] std::io::Error),
    #[error("Data: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("The application database lock is unavailable")]
    Lock,
}

/// Clones share the same connection. Each mutation is serialized and playlist,
/// settings and job transitions use transactions where multiple writes occur.
#[derive(Clone)]
pub struct AppCore {
    db: Arc<Mutex<Connection>>,
    import_lock: Arc<Mutex<()>>,
    profile_dir: Arc<PathBuf>,
    library_dir: Arc<PathBuf>,
}

impl AppCore {
    /// Open a profile; existing managed files are interpreted relative to
    /// `<profile>/Library`. Use `open_with_library` for a legacy library root.
    pub fn open(profile_dir: impl AsRef<Path>) -> Result<Self> {
        let profile_dir = profile_dir.as_ref();
        Self::open_with_library(profile_dir, profile_dir.join("Library"))
    }

    pub fn open_with_library(
        profile_dir: impl AsRef<Path>,
        library_dir: impl AsRef<Path>,
    ) -> Result<Self> {
        std::fs::create_dir_all(profile_dir.as_ref())?;
        std::fs::create_dir_all(library_dir.as_ref())?;
        let profile_dir = std::fs::canonicalize(profile_dir.as_ref())?;
        let library_dir = std::fs::canonicalize(library_dir.as_ref())?;
        let database = profile_dir.join("yt-ultimate.sqlite3");
        let mut db = Connection::open(&database)?;
        db.busy_timeout(Duration::from_secs(10))?;
        db.pragma_update(None, "foreign_keys", true)?;

        let legacy_schema: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')",
            [], |r| r.get(0),
        )?;
        if legacy_schema {
            let version: i64 = db.query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |r| r.get(0),
            )?;
            if version != 4 {
                return Err(CoreError::Invalid(format!(
                    "Unsupported legacy database schema {version}; this build imports schema 4 and the original database was not changed"
                )));
            }
        } else {
            let has_tables: bool=db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%')",[],|row|row.get(0))?;
            if has_tables {
                return Err(CoreError::Invalid(
                    "The existing database has an unrecognized schema; it was not changed".into(),
                ));
            }
        }
        let rust_version = migrations::version(&db)?;
        if rust_version > migrations::LATEST_VERSION {
            return Err(CoreError::Invalid(format!(
                "Database requires a newer Rust application (schema {rust_version}); it was not changed"
            )));
        }
        let initialized = rust_version > 0;
        if !initialized && legacy_schema {
            migrations::backup(
                &db,
                &profile_dir.join(format!("pre-rust-{}.sqlite3", uuid::Uuid::new_v4())),
            )?;
        }
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        let transaction = db.transaction()?;
        transaction.execute_batch(include_str!("schema.sql"))?;
        transaction.commit()?;
        // A legacy profile was just backed up as a whole; an existing Rust
        // profile is backed up by the migrator before its first pending step.
        migrations::migrate(&mut db, initialized.then_some(profile_dir.as_path()))?;

        let core = Self {
            db: Arc::new(Mutex::new(db)),
            import_lock: Arc::new(Mutex::new(())),
            profile_dir: Arc::new(profile_dir),
            library_dir: Arc::new(library_dir),
        };
        core.recover_interrupted_jobs()?;
        core.recover_interrupted_listens()?;
        Ok(core)
    }

    pub fn profile_dir(&self) -> &Path {
        self.profile_dir.as_ref()
    }
    pub fn library_dir(&self) -> &Path {
        self.library_dir.as_ref()
    }

    pub fn integrity_check(&self) -> Result<String> {
        Ok(self
            .connection()?
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))?)
    }

    pub(crate) fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.db.lock().map_err(|_| CoreError::Lock)
    }

    pub fn setting(&self, key: &str) -> Result<Option<serde_json::Value>> {
        let raw: Option<String> = self
            .connection()?
            .query_row("SELECT value_json FROM settings WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        raw.map(|raw| serde_json::from_str(&raw).map_err(CoreError::from))
            .transpose()
    }

    pub fn set_setting(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        if key.trim().is_empty() || key.len() > 240 {
            return Err(CoreError::Invalid(
                "Setting key must contain 1–240 characters".into(),
            ));
        }
        settings::validate_setting(key, value)?;
        self.connection()?.execute(
            "INSERT INTO settings(key,value_json,updated_at) VALUES(?1,?2,?3)
             ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at",
            rusqlite::params![key, serde_json::to_string(value)?, now()],
        )?;
        Ok(())
    }
}

pub(crate) fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
