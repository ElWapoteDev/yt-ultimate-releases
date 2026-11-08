//! In-process application state for the Windows and macOS Rust frontends.
//! No Python runtime, HTTP service or child process is required.

mod artwork;
mod cleanup;
mod jobs;
mod library;
mod models;
mod playlists;
mod settings;

pub use artwork::{MAX_ARTWORK_BYTES, validate_artwork};
pub use models::*;
pub use settings::Settings;

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
        let initialized: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='rust_schema_migrations')", [], |r| r.get(0),
        )?;
        if initialized {
            let version: i64 = db.query_row(
                "SELECT COALESCE(MAX(version),0) FROM rust_schema_migrations",
                [],
                |r| r.get(0),
            )?;
            if version > 2 {
                return Err(CoreError::Invalid(format!(
                    "Database requires a newer Rust application (schema {version}); it was not changed"
                )));
            }
        }
        if !initialized && legacy_schema {
            // SQLite's backup API includes committed WAL pages. A plain file
            // copy would silently lose records from an active WAL database.
            let backup_path =
                profile_dir.join(format!("pre-rust-{}.sqlite3", uuid::Uuid::new_v4()));
            let mut destination = Connection::open(backup_path)?;
            let backup = rusqlite::backup::Backup::new(&db, &mut destination)?;
            backup.run_to_completion(128, Duration::from_millis(10), None)?;
        }
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        let transaction = db.transaction()?;
        transaction.execute_batch(include_str!("schema.sql"))?;
        transaction.commit()?;

        let core = Self {
            db: Arc::new(Mutex::new(db)),
            import_lock: Arc::new(Mutex::new(())),
            profile_dir: Arc::new(profile_dir),
            library_dir: Arc::new(library_dir),
        };
        core.recover_interrupted_jobs()?;
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
