"""Playlist domain service.

Playlists and membership are addressed by stable UUIDs.  Name/filename helpers
exist only as a compatibility bridge for the original frontend.
"""

from __future__ import annotations

import json
import sqlite3
import time
import uuid
from typing import Any

from ..database import Database


class PlaylistNotFoundError(LookupError):
    pass


class TrackNotFoundError(LookupError):
    pass


class PlaylistConflictError(ValueError):
    pass


class PlaylistService:
    """CRUD and ordered membership operations for playlists."""

    def __init__(self, database: Database) -> None:
        self.database = database

    @staticmethod
    def _clean_name(name: str) -> str:
        if not isinstance(name, str):
            raise TypeError("Playlist name must be a string")
        cleaned = " ".join(name.split())
        if not cleaned:
            raise ValueError("Playlist name cannot be blank")
        if len(cleaned) > 160:
            raise ValueError("Playlist name cannot exceed 160 characters")
        return cleaned

    def _playlist_row(self, identifier: str) -> sqlite3.Row:
        if not identifier:
            raise PlaylistNotFoundError("Playlist not found")
        row = self.database.fetchone(
            """
            SELECT *
            FROM playlists
            WHERE id = ? OR name = ? COLLATE NOCASE
            LIMIT 1
            """,
            (identifier, identifier),
        )
        if row is None:
            raise PlaylistNotFoundError(f"Playlist not found: {identifier}")
        return row

    def _track_row(self, identifier: str) -> sqlite3.Row:
        if not identifier:
            raise TrackNotFoundError("Track not found")
        row = self.database.fetchone(
            """
            SELECT *
            FROM tracks
            WHERE id = ?
                OR filename = ? COLLATE NOCASE
                OR relative_path = ? COLLATE NOCASE
            ORDER BY missing ASC, updated_at DESC
            LIMIT 1
            """,
            (identifier, identifier, identifier),
        )
        if row is None:
            raise TrackNotFoundError(f"Track not found: {identifier}")
        return row

    @staticmethod
    def _track_dict(row: sqlite3.Row) -> dict[str, Any]:
        try:
            metadata = json.loads(row["metadata_json"] or "{}")
        except (TypeError, json.JSONDecodeError):
            metadata = {}
        return {
            "id": row["id"],
            "filename": row["filename"],
            "name": row["title"],
            "title": row["title"],
            "artist": row["artist"],
            "channel": row["artist"],
            "album": row["album"],
            "type": row["media_type"],
            "media_type": row["media_type"],
            "relative_path": row["relative_path"],
            "storage_kind": row["storage_kind"],
            "size_bytes": row["size_bytes"],
            "duration": row["duration"],
            "thumbnail": row["artwork_url"],
            "missing": bool(row["missing"]),
            "metadata": metadata,
        }

    def _playlist_dict(
        self,
        row: sqlite3.Row,
        *,
        include_tracks: bool,
    ) -> dict[str, Any]:
        count_row = self.database.fetchone(
            """
            SELECT COUNT(*) AS track_count
            FROM playlist_tracks
            WHERE playlist_id = ?
            """,
            (row["id"],),
        )
        item: dict[str, Any] = {
            "id": row["id"],
            "name": row["name"],
            "description": row["description"],
            "artwork_path": row["artwork_path"],
            "track_count": int(count_row["track_count"]) if count_row else 0,
            "created_at": row["created_at"],
            "updated_at": row["updated_at"],
        }
        if include_tracks:
            tracks = self.database.fetchall(
                """
                SELECT t.*, pt.position, pt.added_at
                FROM playlist_tracks AS pt
                JOIN tracks AS t ON t.id = pt.track_id
                WHERE pt.playlist_id = ?
                ORDER BY pt.position
                """,
                (row["id"],),
            )
            item["tracks"] = [
                {
                    **self._track_dict(track),
                    "position": track["position"],
                    "added_at": track["added_at"],
                }
                for track in tracks
            ]
        return item

    def create(
        self,
        name: str,
        *,
        description: str = "",
        artwork_path: str = "",
    ) -> dict[str, Any]:
        clean_name = self._clean_name(name)
        if not isinstance(description, str):
            raise TypeError("Playlist description must be a string")
        now = time.time()
        playlist_id = str(uuid.uuid4())
        try:
            self.database.execute(
                """
                INSERT INTO playlists(
                    id, name, description, artwork_path, created_at, updated_at
                )
                VALUES (?, ?, ?, ?, ?, ?)
                """,
                (
                    playlist_id,
                    clean_name,
                    description.strip(),
                    str(artwork_path or ""),
                    now,
                    now,
                ),
            )
        except sqlite3.IntegrityError as exc:
            raise PlaylistConflictError(
                f"A playlist named {clean_name!r} already exists"
            ) from exc
        return self.get(playlist_id)

    create_playlist = create

    def list(
        self,
        *,
        include_tracks: bool = False,
    ) -> list[dict[str, Any]]:
        rows = self.database.fetchall(
            "SELECT * FROM playlists ORDER BY name COLLATE NOCASE"
        )
        return [
            self._playlist_dict(row, include_tracks=include_tracks)
            for row in rows
        ]

    list_playlists = list

    def get(
        self,
        playlist_id: str,
        *,
        include_tracks: bool = True,
    ) -> dict[str, Any]:
        row = self._playlist_row(playlist_id)
        return self._playlist_dict(row, include_tracks=include_tracks)

    get_playlist = get

    def update(
        self,
        playlist_id: str,
        *,
        name: str | None = None,
        description: str | None = None,
        artwork_path: str | None = None,
    ) -> dict[str, Any]:
        row = self._playlist_row(playlist_id)
        next_name = row["name"] if name is None else self._clean_name(name)
        if description is not None and not isinstance(description, str):
            raise TypeError("Playlist description must be a string")
        next_description = (
            row["description"] if description is None else description.strip()
        )
        next_artwork = (
            row["artwork_path"]
            if artwork_path is None
            else str(artwork_path)
        )
        try:
            self.database.execute(
                """
                UPDATE playlists
                SET name = ?, description = ?, artwork_path = ?, updated_at = ?
                WHERE id = ?
                """,
                (
                    next_name,
                    next_description,
                    next_artwork,
                    time.time(),
                    row["id"],
                ),
            )
        except sqlite3.IntegrityError as exc:
            raise PlaylistConflictError(
                f"A playlist named {next_name!r} already exists"
            ) from exc
        return self.get(row["id"])

    update_playlist = update

    def delete(self, playlist_id: str) -> bool:
        row = self._playlist_row(playlist_id)
        cursor = self.database.execute(
            "DELETE FROM playlists WHERE id = ?",
            (row["id"],),
        )
        return cursor.rowcount > 0

    delete_playlist = delete

    def add_track(
        self,
        playlist_id: str,
        track_id: str,
        *,
        position: int | None = None,
    ) -> dict[str, Any]:
        playlist = self._playlist_row(playlist_id)
        track = self._track_row(track_id)
        with self.database.transaction():
            existing = self.database.fetchone(
                """
                SELECT position
                FROM playlist_tracks
                WHERE playlist_id = ? AND track_id = ?
                """,
                (playlist["id"], track["id"]),
            )
            if existing is not None:
                return self.get(playlist["id"])

            bounds = self.database.fetchone(
                """
                SELECT COALESCE(MAX(position), -1) AS max_position
                FROM playlist_tracks
                WHERE playlist_id = ?
                """,
                (playlist["id"],),
            )
            size = int(bounds["max_position"]) + 1 if bounds else 0
            target = size if position is None else position
            if type(target) is not int:
                raise TypeError("Playlist position must be an integer")
            if target < 0 or target > size:
                raise ValueError(
                    f"Playlist position must be between 0 and {size}"
                )
            if target < size:
                offset = size + 1
                self.database.execute(
                    """
                    UPDATE playlist_tracks
                    SET position = position + ?
                    WHERE playlist_id = ? AND position >= ?
                    """,
                    (offset, playlist["id"], target),
                )
                self.database.execute(
                    """
                    UPDATE playlist_tracks
                    SET position = position - ? + 1
                    WHERE playlist_id = ? AND position >= ?
                    """,
                    (offset, playlist["id"], target + offset),
                )
            now = time.time()
            self.database.execute(
                """
                INSERT INTO playlist_tracks(
                    playlist_id, track_id, position, added_at
                )
                VALUES (?, ?, ?, ?)
                """,
                (playlist["id"], track["id"], target, now),
            )
            self.database.execute(
                "UPDATE playlists SET updated_at = ? WHERE id = ?",
                (now, playlist["id"]),
            )
        return self.get(playlist["id"])

    def remove_track(
        self,
        playlist_id: str,
        track_id: str,
    ) -> bool:
        playlist = self._playlist_row(playlist_id)
        track = self._track_row(track_id)
        with self.database.transaction():
            membership = self.database.fetchone(
                """
                SELECT position
                FROM playlist_tracks
                WHERE playlist_id = ? AND track_id = ?
                """,
                (playlist["id"], track["id"]),
            )
            if membership is None:
                return False
            self.database.execute(
                """
                DELETE FROM playlist_tracks
                WHERE playlist_id = ? AND track_id = ?
                """,
                (playlist["id"], track["id"]),
            )
            self.database.execute(
                "UPDATE playlists SET updated_at = ? WHERE id = ?",
                (time.time(), playlist["id"]),
            )
        return True

    def reorder_tracks(
        self,
        playlist_id: str,
        track_ids: list[str],
    ) -> dict[str, Any]:
        playlist = self._playlist_row(playlist_id)
        if len(track_ids) != len(set(track_ids)):
            raise ValueError("Track IDs cannot be repeated")
        rows = self.database.fetchall(
            """
            SELECT track_id
            FROM playlist_tracks
            WHERE playlist_id = ?
            ORDER BY position
            """,
            (playlist["id"],),
        )
        current = {row["track_id"] for row in rows}
        if current != set(track_ids):
            raise ValueError(
                "Reorder payload must contain every playlist track exactly once"
            )
        with self.database.transaction():
            offset = len(track_ids) + 1
            self.database.execute(
                """
                UPDATE playlist_tracks
                SET position = position + ?
                WHERE playlist_id = ?
                """,
                (offset, playlist["id"]),
            )
            for position, track_id in enumerate(track_ids):
                self.database.execute(
                    """
                    UPDATE playlist_tracks
                    SET position = ?
                    WHERE playlist_id = ? AND track_id = ?
                    """,
                    (position, playlist["id"], track_id),
                )
            self.database.execute(
                "UPDATE playlists SET updated_at = ? WHERE id = ?",
                (time.time(), playlist["id"]),
            )
        return self.get(playlist["id"])

    def as_legacy_map(self) -> dict[str, list[str]]:
        """Return the ``name -> [filename]`` shape used by the old UI."""

        rows = self.database.fetchall(
            """
            SELECT p.name, t.filename
            FROM playlists AS p
            LEFT JOIN playlist_tracks AS pt ON pt.playlist_id = p.id
            LEFT JOIN tracks AS t ON t.id = pt.track_id
            ORDER BY p.name COLLATE NOCASE, pt.position
            """
        )
        result: dict[str, list[str]] = {}
        for row in rows:
            result.setdefault(row["name"], [])
            if row["filename"] is not None:
                result[row["name"]].append(row["filename"])
        return result

    list_legacy = as_legacy_map

    def add_by_name(
        self,
        playlist_name: str,
        track_id_or_filename: str,
    ) -> dict[str, Any]:
        return self.add_track(playlist_name, track_id_or_filename)

    def remove_by_name(
        self,
        playlist_name: str,
        track_id_or_filename: str,
    ) -> bool:
        return self.remove_track(playlist_name, track_id_or_filename)
