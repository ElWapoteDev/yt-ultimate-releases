"""SQLite persistence for YT Ultimate.

The rest of the application talks to this small wrapper instead of owning
connections or schema details.  A single process-wide connection is protected
by a re-entrant lock; this keeps transactions atomic across service calls while
WAL mode still lets diagnostic/read-only processes inspect the database.
"""

from __future__ import annotations

import json
import sqlite3
import threading
import time
from collections.abc import Iterator, Mapping, Sequence
from contextlib import contextmanager
from pathlib import Path
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from .config import AppConfig


SqlParameters = Sequence[Any] | Mapping[str, Any]


def _json_default(value: Any) -> Any:
    if isinstance(value, Path):
        return str(value)
    if isinstance(value, (set, frozenset, tuple)):
        return list(value)
    raise TypeError(f"{type(value).__name__} is not JSON serializable")


class Database:
    """Thread-safe SQLite database with nested transaction support."""

    SCHEMA_VERSION = 4

    _MIGRATIONS: tuple[tuple[int, str, tuple[str, ...]], ...] = (
        (
            1,
            "initial_schema",
            (
                """
                CREATE TABLE IF NOT EXISTS tracks (
                    id TEXT PRIMARY KEY,
                    storage_kind TEXT NOT NULL DEFAULT 'managed'
                        CHECK (storage_kind IN ('managed', 'reference')),
                    relative_path TEXT,
                    external_path TEXT,
                    filename TEXT NOT NULL,
                    title TEXT NOT NULL,
                    artist TEXT NOT NULL DEFAULT '',
                    album TEXT NOT NULL DEFAULT '',
                    media_type TEXT NOT NULL
                        CHECK (media_type IN ('audio', 'video')),
                    mime_type TEXT NOT NULL DEFAULT '',
                    extension TEXT NOT NULL DEFAULT '',
                    size_bytes INTEGER NOT NULL DEFAULT 0
                        CHECK (size_bytes >= 0),
                    duration REAL NOT NULL DEFAULT 0
                        CHECK (duration >= 0),
                    bitrate INTEGER NOT NULL DEFAULT 0
                        CHECK (bitrate >= 0),
                    sample_rate INTEGER NOT NULL DEFAULT 0
                        CHECK (sample_rate >= 0),
                    source_url TEXT NOT NULL DEFAULT '',
                    source_id TEXT NOT NULL DEFAULT '',
                    artwork_path TEXT NOT NULL DEFAULT '',
                    artwork_url TEXT NOT NULL DEFAULT '',
                    legacy_playlist TEXT NOT NULL DEFAULT '',
                    metadata_json TEXT NOT NULL DEFAULT '{}',
                    missing INTEGER NOT NULL DEFAULT 0
                        CHECK (missing IN (0, 1)),
                    file_modified_at REAL NOT NULL DEFAULT 0,
                    downloaded_at REAL NOT NULL DEFAULT 0,
                    created_at REAL NOT NULL,
                    updated_at REAL NOT NULL,
                    CHECK (
                        (storage_kind = 'managed'
                            AND relative_path IS NOT NULL
                            AND external_path IS NULL)
                        OR
                        (storage_kind = 'reference'
                            AND relative_path IS NULL
                            AND external_path IS NOT NULL)
                    )
                )
                """,
                """
                CREATE TABLE IF NOT EXISTS playlists (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL COLLATE NOCASE UNIQUE,
                    description TEXT NOT NULL DEFAULT '',
                    artwork_path TEXT NOT NULL DEFAULT '',
                    created_at REAL NOT NULL,
                    updated_at REAL NOT NULL
                )
                """,
                """
                CREATE TABLE IF NOT EXISTS playlist_tracks (
                    playlist_id TEXT NOT NULL
                        REFERENCES playlists(id) ON DELETE CASCADE,
                    track_id TEXT NOT NULL
                        REFERENCES tracks(id) ON DELETE CASCADE,
                    position INTEGER NOT NULL CHECK (position >= 0),
                    added_at REAL NOT NULL,
                    PRIMARY KEY (playlist_id, track_id),
                    UNIQUE (playlist_id, position)
                )
                """,
                """
                CREATE TABLE IF NOT EXISTS jobs (
                    id TEXT PRIMARY KEY,
                    kind TEXT NOT NULL,
                    status TEXT NOT NULL,
                    progress REAL NOT NULL DEFAULT 0
                        CHECK (progress >= 0 AND progress <= 100),
                    message TEXT NOT NULL DEFAULT '',
                    payload_json TEXT NOT NULL DEFAULT '{}',
                    result_json TEXT NOT NULL DEFAULT '{}',
                    error TEXT NOT NULL DEFAULT '',
                    cancellable INTEGER NOT NULL DEFAULT 1
                        CHECK (cancellable IN (0, 1)),
                    plugin_id TEXT NOT NULL DEFAULT '',
                    created_at REAL NOT NULL,
                    started_at REAL,
                    finished_at REAL,
                    updated_at REAL NOT NULL
                )
                """,
                """
                CREATE TABLE IF NOT EXISTS settings (
                    key TEXT PRIMARY KEY,
                    value_json TEXT NOT NULL,
                    updated_at REAL NOT NULL
                )
                """,
                """
                CREATE TABLE IF NOT EXISTS plugin_states (
                    plugin_id TEXT PRIMARY KEY,
                    enabled INTEGER NOT NULL DEFAULT 0
                        CHECK (enabled IN (0, 1)),
                    installed_version TEXT NOT NULL DEFAULT '',
                    config_json TEXT NOT NULL DEFAULT '{}',
                    state_json TEXT NOT NULL DEFAULT '{}',
                    permissions_json TEXT NOT NULL DEFAULT '[]',
                    last_error TEXT NOT NULL DEFAULT '',
                    installed_at REAL NOT NULL,
                    updated_at REAL NOT NULL
                )
                """,
            ),
        ),
        (
            2,
            "indexes",
            (
                """
                CREATE UNIQUE INDEX IF NOT EXISTS
                    idx_tracks_relative_path_unique
                ON tracks(relative_path COLLATE NOCASE)
                WHERE relative_path IS NOT NULL
                """,
                """
                CREATE UNIQUE INDEX IF NOT EXISTS
                    idx_tracks_external_path_unique
                ON tracks(external_path)
                WHERE external_path IS NOT NULL
                """,
                """
                CREATE INDEX IF NOT EXISTS idx_tracks_library_listing
                ON tracks(missing, media_type, updated_at DESC)
                """,
                """
                CREATE INDEX IF NOT EXISTS idx_tracks_filename
                ON tracks(filename COLLATE NOCASE)
                """,
                """
                CREATE INDEX IF NOT EXISTS idx_playlist_tracks_order
                ON playlist_tracks(playlist_id, position)
                """,
                """
                CREATE INDEX IF NOT EXISTS idx_jobs_status_updated
                ON jobs(status, updated_at DESC)
                """,
            ),
        ),
        (
            3,
            "playlist_membership_invariants",
            (
                """
                CREATE TRIGGER IF NOT EXISTS
                    trg_playlist_tracks_compact_after_delete
                AFTER DELETE ON playlist_tracks
                BEGIN
                    UPDATE playlist_tracks
                    SET position = position - 1
                    WHERE playlist_id = OLD.playlist_id
                        AND position > OLD.position;
                END
                """,
            ),
        ),
        (
            4,
            "plugins_are_opt_in",
            (
                "ALTER TABLE plugin_states RENAME TO plugin_states_old",
                """
                CREATE TABLE plugin_states (
                    plugin_id TEXT PRIMARY KEY,
                    enabled INTEGER NOT NULL DEFAULT 0
                        CHECK (enabled IN (0, 1)),
                    installed_version TEXT NOT NULL DEFAULT '',
                    config_json TEXT NOT NULL DEFAULT '{}',
                    state_json TEXT NOT NULL DEFAULT '{}',
                    permissions_json TEXT NOT NULL DEFAULT '[]',
                    last_error TEXT NOT NULL DEFAULT '',
                    installed_at REAL NOT NULL,
                    updated_at REAL NOT NULL
                )
                """,
                """
                INSERT INTO plugin_states(
                    plugin_id, enabled, installed_version, config_json,
                    state_json, permissions_json, last_error, installed_at,
                    updated_at
                )
                SELECT
                    plugin_id, enabled, installed_version, config_json,
                    state_json, permissions_json, last_error, installed_at,
                    updated_at
                FROM plugin_states_old
                """,
                "DROP TABLE plugin_states_old",
            ),
        ),
    )

    def __init__(
        self,
        path_or_config: str | Path | AppConfig,
        *,
        timeout: float = 30.0,
        initialize: bool = True,
    ) -> None:
        configured_path = getattr(path_or_config, "database_path", path_or_config)
        self.path = Path(configured_path).expanduser()
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._timeout = timeout
        self._lock = threading.RLock()
        self._transaction_state = threading.local()
        self._closed = False
        self._connection = sqlite3.connect(
            str(self.path),
            timeout=timeout,
            isolation_level=None,
            check_same_thread=False,
        )
        self._connection.row_factory = sqlite3.Row
        self._configure_connection()
        if initialize:
            self.migrate()

    def _configure_connection(self) -> None:
        with self._lock:
            self._connection.execute("PRAGMA foreign_keys = ON")
            self._connection.execute(
                f"PRAGMA busy_timeout = {max(1, int(self._timeout * 1000))}"
            )
            self._connection.execute("PRAGMA journal_mode = WAL")
            self._connection.execute("PRAGMA synchronous = NORMAL")
            self._connection.execute("PRAGMA temp_store = MEMORY")

    def _assert_open(self) -> None:
        if self._closed:
            raise RuntimeError("Database connection is closed")

    @property
    def connection(self) -> sqlite3.Connection:
        """Expose the connection for SQLite APIs that require one.

        Application code should normally prefer :meth:`execute` and
        :meth:`transaction`, which also provide locking.
        """

        self._assert_open()
        return self._connection

    @property
    def schema_version(self) -> int:
        row = self.fetchone(
            "SELECT COALESCE(MAX(version), 0) AS version FROM schema_migrations"
        )
        return int(row["version"]) if row else 0

    def execute(
        self,
        sql: str,
        parameters: SqlParameters = (),
    ) -> sqlite3.Cursor:
        """Execute one statement and return its cursor.

        SQLite is in autocommit mode outside a :meth:`transaction` block.
        """

        with self._lock:
            self._assert_open()
            return self._connection.execute(sql, parameters)

    def executemany(
        self,
        sql: str,
        parameter_rows: Sequence[SqlParameters],
    ) -> sqlite3.Cursor:
        with self._lock:
            self._assert_open()
            return self._connection.executemany(sql, parameter_rows)

    def fetchone(
        self,
        sql: str,
        parameters: SqlParameters = (),
    ) -> sqlite3.Row | None:
        with self._lock:
            self._assert_open()
            return self._connection.execute(sql, parameters).fetchone()

    def fetchall(
        self,
        sql: str,
        parameters: SqlParameters = (),
    ) -> list[sqlite3.Row]:
        with self._lock:
            self._assert_open()
            return list(self._connection.execute(sql, parameters).fetchall())

    @contextmanager
    def transaction(
        self,
        *,
        immediate: bool = True,
    ) -> Iterator[sqlite3.Connection]:
        """Run statements atomically.

        Nested calls use savepoints.  The lock is held for the complete outer
        transaction so another application thread cannot accidentally commit or
        roll back work that it does not own.
        """

        with self._lock:
            self._assert_open()
            depth = int(getattr(self._transaction_state, "depth", 0))
            savepoint = f"ytu_nested_{depth}"
            if depth == 0:
                self._connection.execute(
                    "BEGIN IMMEDIATE" if immediate else "BEGIN"
                )
            else:
                self._connection.execute(f"SAVEPOINT {savepoint}")
            self._transaction_state.depth = depth + 1
            try:
                yield self._connection
            except BaseException:
                if depth == 0:
                    self._connection.rollback()
                else:
                    self._connection.execute(
                        f"ROLLBACK TO SAVEPOINT {savepoint}"
                    )
                    self._connection.execute(f"RELEASE SAVEPOINT {savepoint}")
                raise
            else:
                if depth == 0:
                    self._connection.commit()
                else:
                    self._connection.execute(f"RELEASE SAVEPOINT {savepoint}")
            finally:
                self._transaction_state.depth = depth

    def migrate(self) -> int:
        """Apply every pending idempotent schema migration."""

        self.execute(
            """
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at TEXT NOT NULL
                    DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            )
            """
        )
        applied = {
            int(row["version"])
            for row in self.fetchall("SELECT version FROM schema_migrations")
        }
        if applied and max(applied) > self.SCHEMA_VERSION:
            raise RuntimeError(
                "Database schema is newer than this application build "
                f"({max(applied)} > {self.SCHEMA_VERSION})"
            )
        for version, name, statements in self._MIGRATIONS:
            if version in applied:
                continue
            with self.transaction():
                for statement in statements:
                    self.execute(statement)
                self.execute(
                    """
                    INSERT OR IGNORE INTO schema_migrations(version, name)
                    VALUES (?, ?)
                    """,
                    (version, name),
                )
        return self.schema_version

    def get_setting(self, key: str, default: Any = None) -> Any:
        row = self.fetchone(
            "SELECT value_json FROM settings WHERE key = ?",
            (key,),
        )
        if row is None:
            return default
        try:
            return json.loads(row["value_json"])
        except (TypeError, json.JSONDecodeError):
            return default

    def get_settings(self) -> dict[str, Any]:
        values: dict[str, Any] = {}
        for row in self.fetchall(
            "SELECT key, value_json FROM settings ORDER BY key"
        ):
            try:
                values[row["key"]] = json.loads(row["value_json"])
            except (TypeError, json.JSONDecodeError):
                continue
        return values

    def set_setting(self, key: str, value: Any) -> None:
        if not isinstance(key, str) or not key.strip():
            raise ValueError("Setting key must be a non-empty string")
        encoded = json.dumps(
            value,
            ensure_ascii=False,
            separators=(",", ":"),
            default=_json_default,
        )
        self.execute(
            """
            INSERT INTO settings(key, value_json, updated_at)
            VALUES (?, ?, ?)
            ON CONFLICT(key) DO UPDATE SET
                value_json = excluded.value_json,
                updated_at = excluded.updated_at
            """,
            (key.strip(), encoded, time.time()),
        )

    def set_settings(self, values: Mapping[str, Any]) -> None:
        with self.transaction():
            for key, value in values.items():
                self.set_setting(key, value)

    def delete_setting(self, key: str) -> bool:
        cursor = self.execute("DELETE FROM settings WHERE key = ?", (key,))
        return cursor.rowcount > 0

    def checkpoint(self, *, truncate: bool = False) -> sqlite3.Row | None:
        mode = "TRUNCATE" if truncate else "PASSIVE"
        with self._lock:
            return self._connection.execute(
                f"PRAGMA wal_checkpoint({mode})"
            ).fetchone()

    def integrity_check(self) -> str:
        row = self.fetchone("PRAGMA integrity_check")
        return str(row[0]) if row else "unknown"

    def close(self) -> None:
        with self._lock:
            if self._closed:
                return
            self._connection.close()
            self._closed = True

    def __enter__(self) -> Database:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()
