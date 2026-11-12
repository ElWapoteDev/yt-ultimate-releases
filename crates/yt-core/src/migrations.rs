//! Numbered, transactional migrations for the Rust-owned tables.
//!
//! The legacy v4 baseline (`schema.sql`) is idempotent and runs on every open.
//! Everything newer is a migration here: it runs once, inside a transaction,
//! and its version is recorded in `rust_schema_migrations`.

use crate::{CoreError, Result, now};
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, time::Duration};

struct Migration {
    version: i64,
    sql: &'static str,
}

/// Versions are dense, ascending, and never reused or edited once shipped.
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("migrations/0001_rust_tables.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("migrations/0002_library_exclusions.sql"),
    },
    Migration {
        version: 3,
        sql: include_str!("migrations/0003_listening_and_favorites.sql"),
    },
    Migration {
        version: 4,
        sql: include_str!("migrations/0004_profile_search_catalog.sql"),
    },
    Migration {
        version: 5,
        sql: include_str!("migrations/0005_album_artwork.sql"),
    },
    Migration {
        version: 6,
        sql: include_str!("migrations/0006_dj_analysis.sql"),
    },
];

/// Newest Rust schema this build understands.
pub(crate) const LATEST_VERSION: i64 = MIGRATIONS[MIGRATIONS.len() - 1].version;

/// Pages copied per backup step; small steps keep the source readable.
const BACKUP_PAGES_PER_STEP: i32 = 128;
const BACKUP_STEP_PAUSE: Duration = Duration::from_millis(10);

/// Applied Rust schema version, or 0 when the database has no Rust tables.
pub(crate) fn version(db: &Connection) -> Result<i64> {
    let tracked: Option<String> = db
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='rust_schema_migrations'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if tracked.is_none() {
        return Ok(0);
    }
    Ok(db.query_row(
        "SELECT COALESCE(MAX(version),0) FROM rust_schema_migrations",
        [],
        |row| row.get(0),
    )?)
}

/// Apply every pending migration in order. When `backup_dir` is given and at
/// least one migration is pending, a consistent SQLite backup of the current
/// database is written there first, so a failed or unwanted upgrade is never
/// the only copy of the user's data.
pub(crate) fn migrate(db: &mut Connection, backup_dir: Option<&Path>) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS rust_schema_migrations(version INTEGER PRIMARY KEY,applied_at REAL NOT NULL)",
    )?;
    let current = version(db)?;
    if current > LATEST_VERSION {
        return Err(CoreError::Invalid(format!(
            "Database requires a newer Rust application (schema {current}); it was not changed"
        )));
    }
    let pending = MIGRATIONS
        .iter()
        .filter(|migration| migration.version > current)
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Ok(());
    }
    if let Some(directory) = backup_dir {
        let name = format!(
            "pre-schema-{LATEST_VERSION}-{}.sqlite3",
            uuid::Uuid::new_v4()
        );
        backup(db, &directory.join(name))?;
    }
    for migration in pending {
        let transaction = db.transaction()?;
        transaction.execute_batch(migration.sql)?;
        transaction.execute(
            "INSERT INTO rust_schema_migrations(version,applied_at) VALUES(?1,?2)",
            params![migration.version, now()],
        )?;
        transaction.commit()?;
    }
    Ok(())
}

/// Copy the database with SQLite's backup API, which includes committed WAL
/// pages that a plain file copy would silently lose. The copy is switched to
/// rollback journaling so it stays one self-contained file.
pub(crate) fn backup(db: &Connection, destination: &Path) -> Result<()> {
    let mut target = Connection::open(destination)?;
    {
        let backup = rusqlite::backup::Backup::new(db, &mut target)?;
        backup.run_to_completion(BACKUP_PAGES_PER_STEP, BACKUP_STEP_PAUSE, None)?;
    }
    target.pragma_update(None, "journal_mode", "DELETE")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_dense_and_ascending() {
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            assert_eq!(migration.version, index as i64 + 1);
        }
        assert_eq!(LATEST_VERSION, MIGRATIONS.len() as i64);
    }

    #[test]
    fn fresh_database_reaches_latest_once_without_backup() {
        let directory = tempfile::tempdir().unwrap();
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!("schema.sql")).unwrap();
        migrate(&mut db, None).unwrap();
        migrate(&mut db, Some(directory.path())).unwrap();
        assert_eq!(version(&db).unwrap(), LATEST_VERSION);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        let legacy_tables: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('rust_track_preferences','rust_playback_history')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_tables, 0);
    }

    #[test]
    fn newer_schema_is_refused_without_changes() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!("schema.sql")).unwrap();
        migrate(&mut db, None).unwrap();
        db.execute(
            "INSERT INTO rust_schema_migrations(version,applied_at) VALUES(?1,0)",
            [LATEST_VERSION + 1],
        )
        .unwrap();
        assert!(migrate(&mut db, None).is_err());
        assert_eq!(version(&db).unwrap(), LATEST_VERSION + 1);
    }
}
