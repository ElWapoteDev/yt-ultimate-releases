"""Managed media library and safe filesystem operations."""

from __future__ import annotations

import json
import mimetypes
import os
import re
import shutil
import sqlite3
import subprocess
import time
import uuid
from collections.abc import Iterable, Mapping
from pathlib import Path
from typing import TYPE_CHECKING, Any
from urllib.parse import quote, unquote, urlparse

from ..config import (
    HIDDEN_PROCESS_OPTIONS,
    SUPPORTED_AUDIO_EXTENSIONS,
    SUPPORTED_MEDIA_EXTENSIONS,
    SUPPORTED_PLAYLIST_EXTENSIONS,
)
from ..database import Database

if TYPE_CHECKING:
    from ..config import AppConfig


ARTWORK_EXTENSIONS = (".jpg", ".jpeg", ".png", ".webp", ".avif")
TEMPORARY_SUFFIXES = (".part", ".ytdl", ".temp", ".tmp")
_CONTROL_CHARACTERS = re.compile(r"[\x00-\x1f\x7f]")
# Windows refuses to rename a file that another handle keeps open.
_WINDOWS_FILE_LOCKS = os.name == "nt"


class TrackNotFoundError(LookupError):
    pass


class UnsafeLibraryPathError(ValueError):
    pass


class UnsupportedMediaError(ValueError):
    pass


class MediaInUseError(RuntimeError):
    pass


def _format_size(size_bytes: int) -> str:
    if size_bytes >= 1_073_741_824:
        return f"{size_bytes / 1_073_741_824:.1f} GB"
    if size_bytes >= 1_048_576:
        return f"{size_bytes / 1_048_576:.1f} MB"
    if size_bytes >= 1024:
        return f"{size_bytes / 1024:.0f} KB"
    return f"{size_bytes} B"


def _json_object(raw: str | None) -> dict[str, Any]:
    try:
        value = json.loads(raw or "{}")
    except (TypeError, json.JSONDecodeError):
        return {}
    return value if isinstance(value, dict) else {}


def _float(value: Any, default: float = 0.0) -> float:
    try:
        result = float(value)
    except (TypeError, ValueError):
        return default
    return max(0.0, result)


def _integer(value: Any, default: int = 0) -> int:
    try:
        result = int(value)
    except (TypeError, ValueError):
        return default
    return max(0, result)


class LibraryService:
    """Index, import, resolve, and delete media without leaking path trust."""

    def __init__(
        self,
        config: AppConfig,
        database: Database | None = None,
    ) -> None:
        self.config = config
        self.database = database or Database(config)
        self._owns_database = database is None
        self.library_root = Path(config.library_dir).expanduser().resolve()
        self.artwork_root = Path(config.thumbnail_dir).expanduser().resolve()
        self.library_root.mkdir(parents=True, exist_ok=True)
        self.artwork_root.mkdir(parents=True, exist_ok=True)

    @staticmethod
    def _validate_media(path: Path) -> None:
        if path.suffix.lower() not in SUPPORTED_MEDIA_EXTENSIONS:
            raise UnsupportedMediaError(
                f"Unsupported media extension: {path.suffix or '(none)'}"
            )

    @staticmethod
    def _clean_component(name: str, fallback: str = "media") -> str:
        cleaned = _CONTROL_CHARACTERS.sub("", name)
        cleaned = cleaned.replace("/", "").strip().strip(".")
        return cleaned[:240] or fallback

    @staticmethod
    def _is_hidden_relative(relative: Path) -> bool:
        return any(part.startswith(".") for part in relative.parts)

    @staticmethod
    def _safe_join(
        root: Path,
        relative: str | Path,
        *,
        must_exist: bool = False,
    ) -> Path:
        value = Path(relative)
        if value.is_absolute():
            raise UnsafeLibraryPathError("A relative path is required")
        try:
            candidate = (root / value).resolve(strict=must_exist)
            candidate.relative_to(root.resolve())
        except (OSError, ValueError) as exc:
            raise UnsafeLibraryPathError(
                "Path escapes its configured library root"
            ) from exc
        return candidate

    def _managed_relative_path(self, path: Path) -> str:
        try:
            resolved = path.resolve(strict=True)
            relative = resolved.relative_to(self.library_root)
        except (OSError, ValueError) as exc:
            raise UnsafeLibraryPathError(
                "Managed media must remain inside the library directory"
            ) from exc
        if self._is_hidden_relative(relative):
            raise UnsafeLibraryPathError("Hidden paths are not library media")
        return relative.as_posix()

    def _iter_managed_media(self, *, recursive: bool = True) -> Iterable[Path]:
        artwork_in_library: Path | None = None
        try:
            artwork_in_library = self.artwork_root.relative_to(self.library_root)
        except ValueError:
            pass

        for directory, child_directories, filenames in os.walk(
            self.library_root,
            followlinks=False,
        ):
            current = Path(directory)
            relative_dir = current.relative_to(self.library_root)
            if not recursive and relative_dir != Path("."):
                child_directories[:] = []
                continue
            allowed_directories: list[str] = []
            for child in child_directories:
                relative_child = relative_dir / child
                if child.startswith("."):
                    continue
                if child.lower() in {"thumbnails", "artwork"}:
                    continue
                if (
                    artwork_in_library is not None
                    and relative_child == artwork_in_library
                ):
                    continue
                allowed_directories.append(child)
            child_directories[:] = allowed_directories
            for filename in filenames:
                if filename.startswith("."):
                    continue
                candidate = current / filename
                if candidate.is_symlink():
                    continue
                if candidate.suffix.lower() not in SUPPORTED_MEDIA_EXTENSIONS:
                    continue
                try:
                    self._managed_relative_path(candidate)
                except UnsafeLibraryPathError:
                    continue
                yield candidate

    def _probe(self, path: Path) -> dict[str, Any]:
        executable = str(getattr(self.config, "ffprobe_path", "") or "")
        if not executable:
            return {}
        try:
            completed = subprocess.run(
                [
                    executable,
                    "-v",
                    "error",
                    "-show_entries",
                    (
                        "format=duration,bit_rate:format_tags="
                        "title,artist,album,album_artist,composer:"
                        "stream=codec_type,sample_rate,bit_rate"
                    ),
                    "-of",
                    "json",
                    str(path),
                ],
                capture_output=True,
                check=False,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=12,
                **HIDDEN_PROCESS_OPTIONS,
            )
        except (OSError, subprocess.SubprocessError):
            return {}
        if completed.returncode != 0:
            return {}
        try:
            result = json.loads(completed.stdout)
        except (TypeError, json.JSONDecodeError):
            return {}
        format_info = result.get("format") or {}
        tags = format_info.get("tags") or {}
        streams = result.get("streams") or []
        audio_stream = next(
            (
                item
                for item in streams
                if item.get("codec_type") == "audio"
            ),
            {},
        )
        return {
            "title": tags.get("title") or "",
            "artist": (
                tags.get("artist")
                or tags.get("album_artist")
                or tags.get("composer")
                or ""
            ),
            "album": tags.get("album") or "",
            "duration": _float(format_info.get("duration")),
            "bitrate": _integer(
                format_info.get("bit_rate") or audio_stream.get("bit_rate")
            ),
            "sample_rate": _integer(audio_stream.get("sample_rate")),
        }

    def _artwork_descriptor(
        self,
        value: Any,
        *,
        media_path: Path,
        storage_kind: str,
    ) -> tuple[str, str]:
        if not value:
            return "", ""
        raw = str(value).strip()
        if raw.startswith(("http://", "https://", "data:")):
            return "", raw
        if raw.startswith("/files/"):
            raw = raw.removeprefix("/files/")
            try:
                local = self._safe_join(self.library_root, raw)
            except UnsafeLibraryPathError:
                return "", ""
            return f"library:{local.relative_to(self.library_root).as_posix()}", ""

        candidate = Path(raw).expanduser()
        if not candidate.is_absolute():
            if storage_kind == "managed":
                candidate = media_path.parent / candidate
            else:
                candidate = media_path.parent / candidate
        try:
            resolved = candidate.resolve(strict=False)
        except OSError:
            return "", ""
        if resolved.suffix.lower() not in ARTWORK_EXTENSIONS:
            return "", ""
        try:
            relative = resolved.relative_to(self.library_root)
            return f"library:{relative.as_posix()}", ""
        except ValueError:
            pass
        try:
            relative = resolved.relative_to(self.artwork_root)
            return f"artwork:{relative.as_posix()}", ""
        except ValueError:
            pass
        if storage_kind == "reference":
            try:
                if resolved.parent == media_path.resolve().parent:
                    return f"external:{resolved}", ""
            except OSError:
                pass
        return "", ""

    def _metadata_values(
        self,
        path: Path,
        metadata: Mapping[str, Any],
        *,
        storage_kind: str,
        probe: bool,
    ) -> dict[str, Any]:
        probed = self._probe(path) if probe else {}
        title = (
            metadata.get("title")
            or metadata.get("name")
            or probed.get("title")
            or path.stem
        )
        artist = (
            metadata.get("artist")
            or metadata.get("channel")
            or metadata.get("uploader")
            or probed.get("artist")
            or ""
        )
        artwork_value = (
            metadata.get("artwork_path")
            or metadata.get("thumbnail_local")
            or metadata.get("thumbnail")
            or metadata.get("thumbnail_url")
            or ""
        )
        artwork_path, detected_url = self._artwork_descriptor(
            artwork_value,
            media_path=path,
            storage_kind=storage_kind,
        )
        artwork_url = (
            metadata.get("artwork_url")
            or detected_url
            or (
                metadata.get("thumbnail_url")
                if str(metadata.get("thumbnail_url", "")).startswith(
                    ("http://", "https://", "data:")
                )
                else ""
            )
        )
        stat = path.stat()
        downloaded_at = _float(
            metadata.get("downloaded_at"),
            stat.st_mtime,
        )
        source_identifier = (
            metadata.get("source_id")
            or metadata.get("video_id")
            or metadata.get("youtube_id")
            or ""
        )
        try:
            metadata_json = json.dumps(
                dict(metadata),
                ensure_ascii=False,
                separators=(",", ":"),
                default=str,
            )
        except (TypeError, ValueError):
            metadata_json = "{}"
        return {
            "filename": path.name,
            "title": str(title).strip() or path.stem,
            "artist": str(artist).strip(),
            "album": str(
                metadata.get("album") or probed.get("album") or ""
            ).strip(),
            "media_type": (
                "audio"
                if path.suffix.lower() in SUPPORTED_AUDIO_EXTENSIONS
                else "video"
            ),
            "mime_type": mimetypes.guess_type(path.name)[0] or "",
            "extension": path.suffix.lower(),
            "size_bytes": stat.st_size,
            "duration": _float(
                metadata.get("duration"),
                _float(probed.get("duration")),
            ),
            "bitrate": _integer(
                metadata.get("bitrate"),
                _integer(probed.get("bitrate")),
            ),
            "sample_rate": _integer(
                metadata.get("sample_rate"),
                _integer(probed.get("sample_rate")),
            ),
            "source_url": str(
                metadata.get("source_url") or metadata.get("url") or ""
            ),
            "source_id": str(source_identifier),
            "artwork_path": artwork_path,
            "artwork_url": str(artwork_url or ""),
            "legacy_playlist": str(metadata.get("playlist") or ""),
            "metadata_json": metadata_json,
            "file_modified_at": stat.st_mtime,
            "downloaded_at": downloaded_at,
        }

    def _upsert_path(
        self,
        path: Path,
        *,
        storage_kind: str,
        metadata: Mapping[str, Any] | None = None,
        probe: bool = True,
    ) -> tuple[str, bool, bool]:
        resolved = path.expanduser().resolve(strict=True)
        if not resolved.is_file():
            raise FileNotFoundError(str(path))
        self._validate_media(resolved)
        if storage_kind not in {"managed", "reference"}:
            raise ValueError("storage_kind must be 'managed' or 'reference'")
        relative_path: str | None = None
        external_path: str | None = None
        if storage_kind == "managed":
            relative_path = self._managed_relative_path(resolved)
            existing = self.database.fetchone(
                "SELECT * FROM tracks WHERE relative_path = ? COLLATE NOCASE",
                (relative_path,),
            )
        else:
            external_path = str(resolved)
            existing = self.database.fetchone(
                "SELECT * FROM tracks WHERE external_path = ?",
                (external_path,),
            )

        supplied_metadata = dict(metadata or {})
        if existing is not None and not supplied_metadata:
            supplied_metadata = _json_object(existing["metadata_json"])
        values = self._metadata_values(
            resolved,
            supplied_metadata,
            storage_kind=storage_kind,
            probe=probe,
        )
        now = time.time()
        if existing is None:
            track_id = str(uuid.uuid4())
            self.database.execute(
                """
                INSERT INTO tracks(
                    id, storage_kind, relative_path, external_path,
                    filename, title, artist, album, media_type, mime_type,
                    extension, size_bytes, duration, bitrate, sample_rate,
                    source_url, source_id, artwork_path, artwork_url,
                    legacy_playlist, metadata_json, missing, file_modified_at,
                    downloaded_at, created_at, updated_at
                )
                VALUES (
                    ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                    ?, ?, 0, ?, ?, ?, ?
                )
                """,
                (
                    track_id,
                    storage_kind,
                    relative_path,
                    external_path,
                    values["filename"],
                    values["title"],
                    values["artist"],
                    values["album"],
                    values["media_type"],
                    values["mime_type"],
                    values["extension"],
                    values["size_bytes"],
                    values["duration"],
                    values["bitrate"],
                    values["sample_rate"],
                    values["source_url"],
                    values["source_id"],
                    values["artwork_path"],
                    values["artwork_url"],
                    values["legacy_playlist"],
                    values["metadata_json"],
                    values["file_modified_at"],
                    values["downloaded_at"],
                    now,
                    now,
                ),
            )
            return track_id, True, True

        comparable_keys = (
            "filename",
            "title",
            "artist",
            "album",
            "media_type",
            "mime_type",
            "extension",
            "size_bytes",
            "duration",
            "bitrate",
            "sample_rate",
            "source_url",
            "source_id",
            "artwork_path",
            "artwork_url",
            "legacy_playlist",
            "metadata_json",
            "file_modified_at",
            "downloaded_at",
        )
        changed = bool(existing["missing"]) or any(
            existing[key] != values[key] for key in comparable_keys
        )
        if changed:
            self.database.execute(
                """
                UPDATE tracks
                SET filename = ?, title = ?, artist = ?, album = ?,
                    media_type = ?, mime_type = ?, extension = ?,
                    size_bytes = ?, duration = ?, bitrate = ?, sample_rate = ?,
                    source_url = ?, source_id = ?, artwork_path = ?,
                    artwork_url = ?, legacy_playlist = ?, metadata_json = ?,
                    missing = 0, file_modified_at = ?, downloaded_at = ?,
                    updated_at = ?
                WHERE id = ?
                """,
                (
                    values["filename"],
                    values["title"],
                    values["artist"],
                    values["album"],
                    values["media_type"],
                    values["mime_type"],
                    values["extension"],
                    values["size_bytes"],
                    values["duration"],
                    values["bitrate"],
                    values["sample_rate"],
                    values["source_url"],
                    values["source_id"],
                    values["artwork_path"],
                    values["artwork_url"],
                    values["legacy_playlist"],
                    values["metadata_json"],
                    values["file_modified_at"],
                    values["downloaded_at"],
                    now,
                    existing["id"],
                ),
            )
        return str(existing["id"]), False, changed

    def register_path(
        self,
        path: str | Path,
        *,
        metadata: Mapping[str, Any] | None = None,
        storage_kind: str | None = None,
        probe: bool = True,
    ) -> dict[str, Any]:
        resolved = Path(path).expanduser().resolve(strict=True)
        if storage_kind is None:
            try:
                resolved.relative_to(self.library_root)
                storage_kind = "managed"
            except ValueError:
                storage_kind = "reference"
        track_id, _, _ = self._upsert_path(
            resolved,
            storage_kind=storage_kind,
            metadata=metadata,
            probe=probe,
        )
        return self.get_track(track_id)

    add_file = register_path

    def register_media(
        self,
        path: str | Path,
        metadata: Mapping[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Implement the media-service registrar protocol."""

        return self.register_path(path, metadata=metadata, probe=True)

    register = register_media

    def reconcile(
        self,
        *,
        recursive: bool = True,
        probe: bool = False,
    ) -> dict[str, int]:
        """Bring the SQLite index in line with managed and referenced files."""

        discovered = 0
        added = 0
        updated = 0
        seen_relative_paths: set[str] = set()
        for path in self._iter_managed_media(recursive=recursive):
            discovered += 1
            relative_path = self._managed_relative_path(path)
            seen_relative_paths.add(relative_path.casefold())
            existing = self.database.fetchone(
                """
                SELECT size_bytes, file_modified_at, missing
                FROM tracks
                WHERE relative_path = ? COLLATE NOCASE
                """,
                (relative_path,),
            )
            stat = path.stat()
            should_probe = probe and (
                existing is None
                or bool(existing["missing"])
                or int(existing["size_bytes"]) != stat.st_size
                or float(existing["file_modified_at"]) != stat.st_mtime
            )
            _, created, changed = self._upsert_path(
                path,
                storage_kind="managed",
                probe=should_probe,
            )
            if created:
                added += 1
            elif changed:
                updated += 1

        missing = 0
        indexed = self.database.fetchall(
            """
            SELECT id, storage_kind, relative_path, external_path, missing
            FROM tracks
            """
        )
        now = time.time()
        for row in indexed:
            if row["storage_kind"] == "managed":
                indexed_path = str(row["relative_path"])
                if not recursive and "/" in indexed_path:
                    is_missing = bool(row["missing"])
                else:
                    is_missing = (
                        indexed_path.casefold() not in seen_relative_paths
                    )
            else:
                external = Path(row["external_path"])
                is_missing = (
                    not external.is_file()
                    or external.suffix.lower()
                    not in SUPPORTED_MEDIA_EXTENSIONS
                )
            if is_missing:
                missing += 1
            if bool(row["missing"]) != is_missing:
                self.database.execute(
                    "UPDATE tracks SET missing = ?, updated_at = ? WHERE id = ?",
                    (int(is_missing), now, row["id"]),
                )
                updated += 1
        return {
            "discovered": discovered,
            "added": added,
            "updated": updated,
            "missing": missing,
            "total": len(indexed),
        }

    scan = reconcile
    scan_library = reconcile

    def _local_path_for_row(
        self,
        row: sqlite3.Row,
        *,
        must_exist: bool,
    ) -> Path:
        if row["storage_kind"] == "managed":
            return self._safe_join(
                self.library_root,
                row["relative_path"],
                must_exist=must_exist,
            )
        external = Path(row["external_path"]).expanduser()
        if not external.is_absolute():
            raise UnsafeLibraryPathError(
                "Referenced media must use an absolute path"
            )
        try:
            resolved = external.resolve(strict=must_exist)
        except OSError as exc:
            raise FileNotFoundError(str(external)) from exc
        self._validate_media(resolved)
        return resolved

    def _row_to_track(self, row: sqlite3.Row) -> dict[str, Any]:
        metadata = _json_object(row["metadata_json"])
        local_path_value = ""
        try:
            local_path = self._local_path_for_row(
                row,
                must_exist=False,
            )
            local_path_value = str(local_path)
        except (UnsafeLibraryPathError, OSError, ValueError):
            pass
        if row["storage_kind"] == "managed":
            encoded_path = quote(str(row["relative_path"]), safe="/")
            content_url = f"/files/{encoded_path}"
        else:
            content_url = f"/library/{row['id']}/content"
        thumbnail = row["artwork_url"]
        if not thumbnail and row["artwork_path"]:
            thumbnail = f"/library/{row['id']}/artwork"
        return {
            "id": row["id"],
            "name": row["title"],
            "title": row["title"],
            "filename": row["filename"],
            "path": content_url,
            "local_path": local_path_value,
            "relative_path": row["relative_path"],
            "storage_kind": row["storage_kind"],
            "thumbnail": thumbnail,
            "artwork_url": row["artwork_url"],
            "type": row["media_type"],
            "media_type": row["media_type"],
            "mime_type": row["mime_type"],
            "extension": row["extension"],
            "size": _format_size(int(row["size_bytes"])),
            "size_bytes": int(row["size_bytes"]),
            "channel": row["artist"],
            "artist": row["artist"],
            "album": row["album"],
            "duration": float(row["duration"]),
            "bitrate": int(row["bitrate"]),
            "sample_rate": int(row["sample_rate"]),
            "source_url": row["source_url"],
            "source_id": row["source_id"],
            "downloaded_at": float(row["downloaded_at"]),
            "playlist": row["legacy_playlist"],
            "missing": bool(row["missing"]),
            "created_at": float(row["created_at"]),
            "updated_at": float(row["updated_at"]),
            "metadata": metadata,
        }

    def get_track(
        self,
        track_id: str,
        *,
        include_missing: bool = True,
    ) -> dict[str, Any]:
        sql = "SELECT * FROM tracks WHERE id = ?"
        parameters: tuple[Any, ...] = (track_id,)
        if not include_missing:
            sql += " AND missing = 0"
        row = self.database.fetchone(sql, parameters)
        if row is None:
            raise TrackNotFoundError(f"Track not found: {track_id}")
        return self._row_to_track(row)

    def find_by_filename(
        self,
        filename: str,
        *,
        include_missing: bool = False,
    ) -> list[dict[str, Any]]:
        sql = "SELECT * FROM tracks WHERE filename = ? COLLATE NOCASE"
        if not include_missing:
            sql += " AND missing = 0"
        sql += " ORDER BY updated_at DESC"
        return [
            self._row_to_track(row)
            for row in self.database.fetchall(sql, (filename,))
        ]

    def list_tracks(
        self,
        *,
        include_missing: bool = False,
        refresh: bool = False,
        query: str = "",
        media_type: str | None = None,
        limit: int | None = None,
        offset: int = 0,
        order: str = "newest",
    ) -> list[dict[str, Any]]:
        if refresh:
            self.reconcile()
        if media_type not in {None, "audio", "video"}:
            raise ValueError("media_type must be 'audio' or 'video'")
        if type(offset) is not int or offset < 0:
            raise ValueError("offset must be a non-negative integer")
        if limit is not None and (type(limit) is not int or limit < 1):
            raise ValueError("limit must be a positive integer")
        ordering = {
            "newest": "downloaded_at DESC, filename COLLATE NOCASE",
            "oldest": "downloaded_at ASC, filename COLLATE NOCASE",
            "title": "title COLLATE NOCASE, filename COLLATE NOCASE",
            "largest": "size_bytes DESC, filename COLLATE NOCASE",
        }
        if order not in ordering:
            raise ValueError(f"Unsupported library order: {order}")

        where: list[str] = []
        parameters: list[Any] = []
        if not include_missing:
            where.append("missing = 0")
        if media_type:
            where.append("media_type = ?")
            parameters.append(media_type)
        clean_query = query.strip()
        if clean_query:
            where.append(
                "(title LIKE ? ESCAPE '\\' OR artist LIKE ? ESCAPE '\\' "
                "OR album LIKE ? ESCAPE '\\' OR filename LIKE ? ESCAPE '\\')"
            )
            escaped = (
                clean_query.replace("\\", "\\\\")
                .replace("%", "\\%")
                .replace("_", "\\_")
            )
            pattern = f"%{escaped}%"
            parameters.extend([pattern, pattern, pattern, pattern])
        sql = "SELECT * FROM tracks"
        if where:
            sql += " WHERE " + " AND ".join(where)
        sql += f" ORDER BY {ordering[order]}"
        if limit is not None:
            sql += " LIMIT ? OFFSET ?"
            parameters.extend([limit, offset])
        elif offset:
            sql += " LIMIT -1 OFFSET ?"
            parameters.append(offset)
        return [
            self._row_to_track(row)
            for row in self.database.fetchall(sql, parameters)
        ]

    list = list_tracks
    list_library = list_tracks

    def stats(self, *, refresh: bool = False) -> dict[str, Any]:
        if refresh:
            self.reconcile()
        row = self.database.fetchone(
            """
            SELECT
                COUNT(*) AS total_files,
                COALESCE(SUM(CASE WHEN media_type = 'audio' THEN 1 ELSE 0 END), 0)
                    AS audio_count,
                COALESCE(SUM(CASE WHEN media_type = 'video' THEN 1 ELSE 0 END), 0)
                    AS video_count,
                COALESCE(SUM(size_bytes), 0) AS total_size_bytes,
                COALESCE(SUM(CASE WHEN storage_kind = 'reference' THEN 1 ELSE 0 END), 0)
                    AS reference_count
            FROM tracks
            WHERE missing = 0
            """
        )
        total_size = int(row["total_size_bytes"]) if row else 0
        missing_row = self.database.fetchone(
            "SELECT COUNT(*) AS count FROM tracks WHERE missing = 1"
        )
        return {
            "total_files": int(row["total_files"]) if row else 0,
            "audio_count": int(row["audio_count"]) if row else 0,
            "video_count": int(row["video_count"]) if row else 0,
            "reference_count": int(row["reference_count"]) if row else 0,
            "missing_count": int(missing_row["count"]) if missing_row else 0,
            "total_size": _format_size(total_size),
            "total_size_bytes": total_size,
        }

    library_stats = stats

    @staticmethod
    def _reserve_unique_destination(destination: Path) -> Path:
        suffix = "".join(destination.suffixes)
        stem = destination.name[: -len(suffix)] if suffix else destination.name
        index = 1
        while True:
            candidate = (
                destination
                if index == 1
                else destination.with_name(f"{stem} ({index}){suffix}")
            )
            try:
                descriptor = os.open(
                    candidate,
                    os.O_CREAT | os.O_EXCL | os.O_WRONLY,
                    0o644,
                )
            except FileExistsError:
                index += 1
                continue
            else:
                os.close(descriptor)
                return candidate

    def _expand_imports(
        self,
        paths: Iterable[str | Path],
        *,
        recursive: bool,
    ) -> tuple[
        list[tuple[Path, Path]],
        list[dict[str, str]],
        dict[str, list[str]],
    ]:
        candidates: list[tuple[Path, Path]] = []
        errors: list[dict[str, str]] = []
        seen: set[str] = set()
        parsed_playlists: set[str] = set()
        playlists: dict[str, list[str]] = {}

        def source_key(path: Path) -> str:
            return os.path.normcase(str(path.resolve()))

        def add_media(item: Path, hint: Path) -> str | None:
            if item.suffix.lower() not in SUPPORTED_MEDIA_EXTENSIONS:
                return None
            try:
                resolved = item.resolve(strict=True)
            except OSError as exc:
                errors.append({"path": str(item), "error": str(exc)})
                return None
            if not resolved.is_file():
                errors.append({"path": str(resolved), "error": "Not a regular file"})
                return None
            key = source_key(resolved)
            if key not in seen:
                seen.add(key)
                candidates.append((resolved, hint))
            return key

        def parse_playlist(playlist_path: Path) -> None:
            playlist_key = source_key(playlist_path)
            if playlist_key in parsed_playlists:
                return
            parsed_playlists.add(playlist_key)
            playlist_name = " ".join(
                _CONTROL_CHARACTERS.sub("", playlist_path.stem).split()
            )[:160] or "Playlist"
            membership = playlists.setdefault(playlist_name, [])
            try:
                content = playlist_path.read_text(
                    encoding="utf-8-sig",
                    errors="replace",
                )
            except OSError as exc:
                errors.append({"path": str(playlist_path), "error": str(exc)})
                return

            for index, raw_line in enumerate(content.splitlines()):
                if index >= 10_000:
                    errors.append(
                        {
                            "path": str(playlist_path),
                            "error": "Playlist exceeds the 10000-entry safety limit",
                        }
                    )
                    break
                value = raw_line.strip()
                if not value or value.startswith("#"):
                    continue
                parsed = urlparse(value)
                if parsed.scheme == "file":
                    item = Path(unquote(parsed.path))
                elif parsed.scheme:
                    errors.append(
                        {
                            "path": value,
                            "error": "Remote playlist entries cannot be imported",
                        }
                    )
                    continue
                else:
                    item = Path(value).expanduser()
                    if not item.is_absolute():
                        item = playlist_path.parent / item
                if item.suffix.lower() in SUPPORTED_PLAYLIST_EXTENSIONS:
                    try:
                        nested = item.resolve(strict=True)
                    except OSError as exc:
                        errors.append({"path": str(item), "error": str(exc)})
                        continue
                    parse_playlist(nested)
                    continue
                key = add_media(
                    item,
                    Path(playlist_name) / item.name,
                )
                if key and key not in membership:
                    membership.append(key)

        for raw_path in paths:
            supplied = Path(raw_path).expanduser()
            try:
                source = supplied.resolve(strict=True)
            except OSError as exc:
                errors.append({"path": str(supplied), "error": str(exc)})
                continue
            if source.is_file():
                expanded = [(source, Path(source.name))]
            elif source.is_dir():
                iterator = source.rglob("*") if recursive else source.iterdir()
                expanded = sorted(
                    [
                        (item, Path(source.name) / item.relative_to(source))
                        for item in iterator
                        if item.is_file()
                    ],
                    key=lambda pair: (
                        pair[0].suffix.lower() not in SUPPORTED_PLAYLIST_EXTENSIONS,
                        str(pair[0]).casefold(),
                    ),
                )
            else:
                errors.append(
                    {"path": str(source), "error": "Not a regular file or folder"}
                )
                continue
            for item, hint in expanded:
                if item.suffix.lower() in SUPPORTED_PLAYLIST_EXTENSIONS:
                    parse_playlist(item)
                else:
                    add_media(item, hint)
        return candidates, errors, playlists

    def _safe_import_hint(self, hint: Path) -> Path:
        parts = [
            self._clean_component(part, "media")
            for part in hint.parts
            if part not in {"", ".", ".."}
        ]
        if not parts:
            raise UnsafeLibraryPathError("Import destination is empty")
        return Path(*parts)

    def import_paths(
        self,
        paths: Iterable[str | Path],
        *,
        mode: str = "copy",
        recursive: bool = True,
        probe: bool = True,
    ) -> dict[str, Any]:
        """Import files/folders by copying or explicitly referencing them.

        ``move`` is supported only when deliberately requested; drag-and-drop
        and all callers that omit ``mode`` are non-destructive copies.
        """

        if mode not in {"copy", "reference", "move"}:
            raise ValueError("mode must be 'copy', 'reference', or 'move'")
        if isinstance(paths, (str, Path)):
            paths = [paths]
        candidates, errors, requested_playlists = self._expand_imports(
            paths,
            recursive=recursive,
        )
        imported: list[dict[str, Any]] = []
        source_tracks: dict[str, str] = {}
        for source, hint in candidates:
            destination: Path | None = None
            moved = False
            try:
                try:
                    source.relative_to(self.library_root)
                    already_managed = True
                except ValueError:
                    already_managed = False

                if mode == "reference" and not already_managed:
                    track = self.register_path(
                        source,
                        storage_kind="reference",
                        probe=probe,
                    )
                elif already_managed:
                    track = self.register_path(
                        source,
                        storage_kind="managed",
                        probe=probe,
                    )
                else:
                    safe_hint = self._safe_import_hint(hint)
                    destination = self._safe_join(
                        self.library_root,
                        safe_hint,
                    )
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    destination = self._safe_join(
                        self.library_root,
                        destination.relative_to(self.library_root),
                    )
                    destination = self._reserve_unique_destination(destination)
                    try:
                        if mode == "move":
                            shutil.move(str(source), str(destination))
                            moved = True
                        else:
                            shutil.copy2(source, destination)
                        track = self.register_path(
                            destination,
                            storage_kind="managed",
                            probe=probe,
                        )
                    except BaseException:
                        if moved and destination.exists():
                            shutil.move(str(destination), str(source))
                        elif destination.exists():
                            destination.unlink()
                        raise
                imported.append(track)
                source_tracks[os.path.normcase(str(source.resolve()))] = track["id"]
            except (
                OSError,
                ValueError,
                sqlite3.DatabaseError,
                subprocess.SubprocessError,
            ) as exc:
                errors.append({"path": str(source), "error": str(exc)})
        return {
            "mode": mode,
            "imported": len(imported),
            "skipped": max(0, len(candidates) - len(imported)),
            "errors": errors,
            "tracks": imported,
            "playlists": {
                name: [
                    source_tracks[source]
                    for source in sources
                    if source in source_tracks
                ]
                for name, sources in requested_playlists.items()
            },
        }

    import_items = import_paths

    def import_file(
        self,
        path: str | Path,
        *,
        mode: str = "copy",
        probe: bool = True,
    ) -> dict[str, Any]:
        result = self.import_paths(
            [path],
            mode=mode,
            recursive=False,
            probe=probe,
        )
        if not result["tracks"]:
            message = (
                result["errors"][0]["error"]
                if result["errors"]
                else "No supported media was imported"
            )
            raise ValueError(message)
        return result["tracks"][0]

    def resolve_content(
        self,
        track_id: str,
        *,
        must_exist: bool = True,
    ) -> Path:
        row = self.database.fetchone(
            "SELECT * FROM tracks WHERE id = ?",
            (track_id,),
        )
        if row is None:
            raise TrackNotFoundError(f"Track not found: {track_id}")
        path = self._local_path_for_row(row, must_exist=must_exist)
        self._validate_media(path)
        if must_exist and not path.is_file():
            raise FileNotFoundError(str(path))
        return path

    resolve_media = resolve_content
    content_path = resolve_content

    def _resolve_artwork_descriptor(
        self,
        descriptor: str,
        row: sqlite3.Row,
    ) -> Path | None:
        if not descriptor:
            return None
        prefix, separator, raw_path = descriptor.partition(":")
        if not separator:
            return None
        try:
            if prefix == "library":
                candidate = self._safe_join(
                    self.library_root,
                    raw_path,
                    must_exist=True,
                )
            elif prefix == "artwork":
                candidate = self._safe_join(
                    self.artwork_root,
                    raw_path,
                    must_exist=True,
                )
            elif prefix == "external" and row["storage_kind"] == "reference":
                candidate = Path(raw_path).resolve(strict=True)
                media = self._local_path_for_row(row, must_exist=False)
                if candidate.parent != media.resolve(strict=False).parent:
                    return None
            else:
                return None
        except (OSError, UnsafeLibraryPathError, ValueError):
            return None
        if (
            candidate.is_file()
            and candidate.suffix.lower() in ARTWORK_EXTENSIONS
        ):
            return candidate
        return None

    def resolve_artwork(self, track_id: str) -> Path | None:
        row = self.database.fetchone(
            "SELECT * FROM tracks WHERE id = ?",
            (track_id,),
        )
        if row is None:
            raise TrackNotFoundError(f"Track not found: {track_id}")
        described = self._resolve_artwork_descriptor(
            row["artwork_path"],
            row,
        )
        if described:
            return described
        try:
            media = self._local_path_for_row(row, must_exist=False)
        except (OSError, UnsafeLibraryPathError):
            return None
        candidates = [
            media.with_suffix(extension) for extension in ARTWORK_EXTENSIONS
        ]
        candidates.extend(
            self.artwork_root / f"{media.stem}{extension}"
            for extension in ARTWORK_EXTENSIONS
        )
        legacy_thumbnails = self.library_root / "thumbnails"
        candidates.extend(
            legacy_thumbnails / f"{media.stem}{extension}"
            for extension in ARTWORK_EXTENSIONS
        )
        for candidate in candidates:
            try:
                resolved = candidate.resolve(strict=True)
            except OSError:
                continue
            if row["storage_kind"] == "reference":
                try:
                    if (
                        resolved.parent != media.resolve(strict=False).parent
                        and self.artwork_root not in resolved.parents
                    ):
                        continue
                except OSError:
                    continue
            else:
                try:
                    resolved.relative_to(self.library_root)
                except ValueError:
                    try:
                        resolved.relative_to(self.artwork_root)
                    except ValueError:
                        continue
            if (
                resolved.is_file()
                and resolved.suffix.lower() in ARTWORK_EXTENSIONS
            ):
                return resolved
        return None

    artwork_path = resolve_artwork

    def delete_track(
        self,
        track_id: str,
        *,
        delete_file: bool = True,
        delete_external: bool = False,
    ) -> dict[str, Any]:
        row = self.database.fetchone(
            "SELECT * FROM tracks WHERE id = ?",
            (track_id,),
        )
        if row is None:
            raise TrackNotFoundError(f"Track not found: {track_id}")
        file_deleted = False
        quarantined_path: Path | None = None
        original_path: Path | None = None
        if delete_file and (
            row["storage_kind"] == "managed" or delete_external
        ):
            path = self._local_path_for_row(row, must_exist=False)
            if path.exists():
                if not path.is_file():
                    raise UnsafeLibraryPathError(
                        "Refusing to delete a non-file library path"
                    )
                original_path = path
                quarantined_path = path.with_name(
                    f".{path.name}.ytu-delete-{uuid.uuid4().hex}"
                )
                self._quarantine(path, quarantined_path)
        try:
            self.database.execute(
                "DELETE FROM tracks WHERE id = ?",
                (track_id,),
            )
        except BaseException:
            if (
                quarantined_path is not None
                and original_path is not None
                and quarantined_path.exists()
            ):
                os.replace(quarantined_path, original_path)
            raise
        if quarantined_path is not None:
            quarantined_path.unlink(missing_ok=True)
            file_deleted = True
        return {
            "status": "deleted",
            "id": track_id,
            "file_deleted": file_deleted,
        }

    delete = delete_track

    @staticmethod
    def _quarantine(path: Path, target: Path) -> None:
        if not _WINDOWS_FILE_LOCKS:
            os.replace(path, target)
            return
        # The track streaming to the player keeps its file open. Requests that
        # are finishing get a short grace period before the conflict is reported.
        for attempt in range(5):
            try:
                os.replace(path, target)
                return
            except PermissionError as exc:
                if attempt == 4:
                    raise MediaInUseError(
                        "El archivo está en uso. Detén la reproducción e inténtalo de nuevo."
                    ) from exc
                time.sleep(0.2)

    @staticmethod
    def _read_legacy_json(path: Path, default: Any) -> Any:
        try:
            if not path.is_file() or path.stat().st_size > 50 * 1024 * 1024:
                return default
            with path.open("r", encoding="utf-8") as handle:
                return json.load(handle)
        except (OSError, UnicodeError, json.JSONDecodeError):
            return default

    def migrate_legacy_json(
        self,
        *,
        metadata_path: str | Path | None = None,
        playlists_path: str | Path | None = None,
        settings_path: str | Path | None = None,
    ) -> dict[str, int]:
        """Import the original JSON stores without modifying their files."""

        scan_result = self.reconcile(probe=False)
        metadata_file = Path(
            metadata_path or self.library_root / "metadata.json"
        )
        playlists_file = Path(
            playlists_path or self.library_root / "playlists.json"
        )
        settings_file = Path(
            settings_path or self.library_root / "settings.json"
        )
        metadata = self._read_legacy_json(metadata_file, {})
        playlists = self._read_legacy_json(playlists_file, {})
        settings = self._read_legacy_json(settings_file, {})
        migrated_metadata = 0
        if isinstance(metadata, dict):
            for filename, info in metadata.items():
                if not isinstance(filename, str) or not isinstance(info, dict):
                    continue
                rows = self.database.fetchall(
                    """
                    SELECT *
                    FROM tracks
                    WHERE filename = ? COLLATE NOCASE
                    ORDER BY missing ASC, updated_at DESC
                    """,
                    (filename,),
                )
                for row in rows:
                    try:
                        path = self._local_path_for_row(row, must_exist=True)
                        self._upsert_path(
                            path,
                            storage_kind=row["storage_kind"],
                            metadata=info,
                            probe=False,
                        )
                        migrated_metadata += 1
                    except (OSError, ValueError):
                        continue

        migrated_playlists = 0
        migrated_memberships = 0
        if isinstance(playlists, dict):
            from .playlists import (
                PlaylistConflictError,
                PlaylistService,
                TrackNotFoundError as PlaylistTrackNotFoundError,
            )

            playlist_service = PlaylistService(self.database)
            for name, filenames in playlists.items():
                if not isinstance(name, str) or not isinstance(
                    filenames,
                    list,
                ):
                    continue
                try:
                    playlist = playlist_service.create(name)
                    migrated_playlists += 1
                except PlaylistConflictError:
                    try:
                        playlist = playlist_service.get(name)
                    except LookupError:
                        continue
                except (TypeError, ValueError):
                    continue
                for filename in filenames:
                    if not isinstance(filename, str):
                        continue
                    try:
                        before = playlist_service.get(
                            playlist["id"],
                            include_tracks=True,
                        )["track_count"]
                        after = playlist_service.add_track(
                            playlist["id"],
                            filename,
                        )
                        if after["track_count"] > before:
                            migrated_memberships += 1
                    except PlaylistTrackNotFoundError:
                        continue

        migrated_settings = 0
        if isinstance(settings, dict):
            valid_legacy_values = {
                key: value
                for key, value in settings.items()
                if isinstance(key, str) and key.strip()
            }
            self.database.set_settings(valid_legacy_values)
            migrated_settings = len(valid_legacy_values)
        return {
            "scanned": scan_result["discovered"],
            "metadata": migrated_metadata,
            "playlists": migrated_playlists,
            "playlist_memberships": migrated_memberships,
            "settings": migrated_settings,
        }

    migrate_legacy = migrate_legacy_json

    def clean_temporary_files(self) -> dict[str, Any]:
        removed: list[str] = []
        for path in self.library_root.iterdir():
            if not path.is_file():
                continue
            is_named_temporary = path.name.startswith(
                ("_v_", "_a_", "_tmpv_", "_tmpa_")
            )
            if (
                path.suffix.lower() not in TEMPORARY_SUFFIXES
                and not is_named_temporary
            ):
                continue
            try:
                path.unlink()
                removed.append(path.name)
            except OSError:
                continue
        return {"cleaned": len(removed), "files": removed}

    def close(self) -> None:
        if self._owns_database:
            self.database.close()
