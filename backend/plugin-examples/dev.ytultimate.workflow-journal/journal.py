"""Thread-safe JSONL journal used by the Workflow Journal example."""

from __future__ import annotations

import json
import math
import os
import tempfile
import threading
import uuid
from collections import Counter, OrderedDict
from collections.abc import Mapping, Sequence
from datetime import UTC, datetime
from pathlib import Path
from typing import Any


SCHEMA_VERSION = 1
REDACTED_VALUE = "[REDACTED]"
MAX_NESTING_DEPTH = 32


def _utc_now() -> str:
    return datetime.now(UTC).isoformat(timespec="milliseconds")


def _json_safe(
    value: Any,
    redacted_keys: frozenset[str],
    *,
    depth: int = 0,
    seen: set[int] | None = None,
) -> Any:
    """Return strict JSON data while recursively redacting matching keys."""

    if depth > MAX_NESTING_DEPTH:
        return "[MAX_DEPTH]"
    if value is None or isinstance(value, (str, bool, int)):
        return value
    if isinstance(value, float):
        return value if math.isfinite(value) else None
    if isinstance(value, Path):
        return str(value)
    if isinstance(value, bytes):
        return f"[BYTES:{len(value)}]"

    seen = set() if seen is None else seen
    if isinstance(value, Mapping):
        identity = id(value)
        if identity in seen:
            return "[CIRCULAR]"
        seen.add(identity)
        try:
            result: dict[str, Any] = {}
            for raw_key, item in value.items():
                key = str(raw_key)
                result[key] = (
                    REDACTED_VALUE
                    if key.casefold() in redacted_keys
                    else _json_safe(
                        item,
                        redacted_keys,
                        depth=depth + 1,
                        seen=seen,
                    )
                )
            return result
        finally:
            seen.remove(identity)

    if isinstance(value, Sequence) and not isinstance(value, str):
        identity = id(value)
        if identity in seen:
            return "[CIRCULAR]"
        seen.add(identity)
        try:
            return [
                _json_safe(
                    item,
                    redacted_keys,
                    depth=depth + 1,
                    seen=seen,
                )
                for item in value
            ]
        finally:
            seen.remove(identity)

    if isinstance(value, (set, frozenset)):
        return sorted(
            (
                _json_safe(
                    item,
                    redacted_keys,
                    depth=depth + 1,
                    seen=seen,
                )
                for item in value
            ),
            key=lambda item: repr(item),
        )
    return str(value)


def _atomic_write_text(path: Path, content: str) -> None:
    """Replace one file atomically using a private temporary sibling."""

    path.parent.mkdir(parents=True, exist_ok=True)
    temporary_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            "w",
            encoding="utf-8",
            dir=path.parent,
            prefix=f".{path.name}.",
            suffix=".tmp",
            delete=False,
        ) as temporary:
            temporary_path = Path(temporary.name)
            temporary.write(content)
            temporary.flush()
            os.fsync(temporary.fileno())
        os.chmod(temporary_path, 0o600)
        os.replace(temporary_path, path)
        temporary_path = None
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)


def _json_line(value: Mapping[str, Any]) -> str:
    return json.dumps(
        value,
        ensure_ascii=False,
        allow_nan=False,
        separators=(",", ":"),
        sort_keys=True,
    )


def _clone_json(value: Any) -> Any:
    """Return a detached JSON-compatible copy."""

    return _json_safe(value, frozenset())


class WorkflowJournal:
    """Persistent journal with bounded retention and atomic metadata."""

    def __init__(self, data_dir: Path | str, *, logger: Any = None) -> None:
        self._logger = logger
        requested_data_dir = Path(data_dir).expanduser()
        if requested_data_dir.is_symlink():
            raise RuntimeError("workflow journal data directory cannot be a symlink")
        requested_data_dir.mkdir(parents=True, exist_ok=True)
        self.data_dir = requested_data_dir.resolve()
        self._make_directory_private(self.data_dir)
        self.journal_path = self.data_dir / "workflow-journal.jsonl"
        self.state_path = self.data_dir / "workflow-journal-state.json"
        self.export_dir = self.data_dir / "exports"
        if self.export_dir.is_symlink():
            raise RuntimeError("workflow journal export directory cannot be a symlink")
        self.export_dir.mkdir(parents=True, exist_ok=True)
        self._make_directory_private(self.export_dir)
        for private_file in (self.journal_path, self.state_path):
            if private_file.is_symlink():
                raise RuntimeError(
                    f"workflow journal file cannot be a symlink: {private_file.name}"
                )
            if private_file.exists():
                self._make_file_private(private_file)
        self._lock = threading.RLock()
        self._closed = False
        self._last_download_progress: OrderedDict[str, float] = OrderedDict()
        self._entries, corrupt_lines = self._load_entries()
        self._state = self._load_state()
        self._reconcile_state(corrupt_lines=corrupt_lines)
        with self._lock:
            self._write_state_unlocked()

    def record(self, event: Any, config: Mapping[str, Any]) -> dict[str, Any] | None:
        """Record one enabled event and return its stored envelope."""

        enabled_events = frozenset(str(name) for name in config.get("events", ()))
        redacted_keys = frozenset(
            str(key).casefold() for key in config.get("redact_keys", ())
        )
        max_entries = int(config.get("max_entries", 5000))
        progress_step = float(config.get("progress_step", 5))
        include_payload = bool(config.get("include_payload", True))

        with self._lock:
            self._ensure_open()
            if event.name == "download.queued":
                self._last_download_progress.pop(
                    self._download_identity(event.payload),
                    None,
                )
            if event.name == "download.media_ready":
                self._last_download_progress.pop(
                    self._download_identity(event.payload),
                    None,
                )
            if event.name not in enabled_events:
                return None
            if event.name == "download.progress" and self._throttle_progress(
                event.payload,
                progress_step,
                tracker_limit=min(max(max_entries, 100), 10000),
            ):
                self._state["throttled_progress"] += 1
                return None

            sequence = int(self._state["total_recorded"]) + 1
            entry: dict[str, Any] = {
                "schema_version": SCHEMA_VERSION,
                "sequence": sequence,
                "event": str(event.name),
                "event_id": str(event.event_id),
                "source": str(event.source) if event.source is not None else None,
                "emitted_at": float(event.emitted_at),
                "recorded_at": _utc_now(),
            }
            if include_payload:
                entry["payload"] = _json_safe(event.payload, redacted_keys)

            self._append_unlocked(entry)
            self._entries.append(entry)
            self._state["total_recorded"] = sequence
            self._state["updated_at"] = _utc_now()
            self._apply_retention_unlocked(max_entries)
            self._reconcile_retained_state_unlocked()
            self._write_state_unlocked()
            return _clone_json(entry)

    def status(self, config: Mapping[str, Any]) -> dict[str, Any]:
        """Return current runtime, storage and retention diagnostics."""

        with self._lock:
            self._ensure_open()
            self._apply_retention_unlocked(int(config.get("max_entries", 5000)))
            self._reconcile_retained_state_unlocked()
            self._write_state_unlocked()
            result = {
                "schema_version": SCHEMA_VERSION,
                "closed": self._closed,
                "journal_path": str(self.journal_path),
                "state_path": str(self.state_path),
                "export_directory": str(self.export_dir),
                "journal_bytes": (
                    self.journal_path.stat().st_size
                    if self.journal_path.is_file()
                    else 0
                ),
                "stored_entries": self._state["stored_entries"],
                "total_recorded": self._state["total_recorded"],
                "throttled_progress": self._state["throttled_progress"],
                "corrupt_lines": self._state["corrupt_lines"],
                "event_counts": dict(self._state["event_counts"]),
                "last_event": self._state["last_event"],
                "last_export": self._state["last_export"],
                "created_at": self._state["created_at"],
                "updated_at": self._state["updated_at"],
                "effective_config": _json_safe(config, frozenset()),
            }
            return _clone_json(result)

    def summary(self, config: Mapping[str, Any]) -> dict[str, Any]:
        """Return a compact summary suitable for a custom event payload."""

        status = self.status(config)
        return {
            "schema_version": status["schema_version"],
            "stored_entries": status["stored_entries"],
            "total_recorded": status["total_recorded"],
            "throttled_progress": status["throttled_progress"],
            "corrupt_lines": status["corrupt_lines"],
            "event_counts": status["event_counts"],
            "last_event": status["last_event"],
            "updated_at": status["updated_at"],
        }

    def recent(
        self,
        *,
        limit: int,
        event_name: str | None = None,
        source: str | None = None,
    ) -> list[dict[str, Any]]:
        """Return newest entries first with optional exact filters."""

        with self._lock:
            self._ensure_open()
            selected: list[dict[str, Any]] = []
            for entry in reversed(self._entries):
                if event_name is not None and entry.get("event") != event_name:
                    continue
                if source is not None and entry.get("source") != source:
                    continue
                selected.append(_clone_json(entry))
                if len(selected) >= limit:
                    break
            return selected

    def export(
        self,
        *,
        limit: int | None = None,
        event_name: str | None = None,
        source: str | None = None,
    ) -> dict[str, Any]:
        """Create an atomic, private JSONL snapshot in the plugin data directory."""

        with self._lock:
            self._ensure_open()
            if self.export_dir.is_symlink():
                raise RuntimeError(
                    "workflow journal export directory cannot be a symlink"
                )
            self.export_dir.mkdir(parents=True, exist_ok=True)
            self._make_directory_private(self.export_dir)
            selected = [
                entry
                for entry in self._entries
                if (event_name is None or entry.get("event") == event_name)
                and (source is None or entry.get("source") == source)
            ]
            if limit is not None:
                selected = selected[-limit:]

            stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%S.%fZ")
            destination = self.export_dir / (
                f"workflow-journal-{stamp}-{uuid.uuid4().hex[:8]}.jsonl"
            )
            content = "".join(f"{_json_line(entry)}\n" for entry in selected)
            _atomic_write_text(destination, content)
            details = {
                "path": str(destination),
                "entries": len(selected),
                "bytes": destination.stat().st_size,
                "created_at": _utc_now(),
                "filters": {
                    "event": event_name,
                    "source": source,
                    "limit": limit,
                },
            }
            self._state["last_export"] = _clone_json(details)
            self._state["updated_at"] = _utc_now()
            self._write_state_unlocked()
            return _clone_json(details)

    def clear(self) -> dict[str, Any]:
        """Atomically clear retained entries and reset counters."""

        with self._lock:
            self._ensure_open()
            removed = len(self._entries)
            _atomic_write_text(self.journal_path, "")
            self._entries.clear()
            self._last_download_progress.clear()
            created_at = self._state["created_at"]
            self._state = self._new_state(created_at=created_at)
            self._state["updated_at"] = _utc_now()
            self._write_state_unlocked()
            return {
                "removed_entries": removed,
                "journal_path": str(self.journal_path),
                "cleared_at": self._state["updated_at"],
            }

    def close(self) -> None:
        """Persist pending metadata and reject future operations."""

        with self._lock:
            if self._closed:
                return
            self._state["updated_at"] = _utc_now()
            self._write_state_unlocked()
            self._last_download_progress.clear()
            self._closed = True

    def _append_unlocked(self, entry: Mapping[str, Any]) -> None:
        descriptor = os.open(
            self.journal_path,
            os.O_APPEND | os.O_CREAT | os.O_WRONLY,
            0o600,
        )
        with os.fdopen(descriptor, "a", encoding="utf-8") as stream:
            stream.write(f"{_json_line(entry)}\n")
            stream.flush()
            os.fsync(stream.fileno())

    def _apply_retention_unlocked(self, max_entries: int) -> None:
        if len(self._entries) <= max_entries:
            return
        self._entries = self._entries[-max_entries:]
        content = "".join(f"{_json_line(entry)}\n" for entry in self._entries)
        _atomic_write_text(self.journal_path, content)

    def _load_entries(self) -> tuple[list[dict[str, Any]], int]:
        if not self.journal_path.is_file():
            return [], 0
        entries: list[dict[str, Any]] = []
        corrupt_lines = 0
        try:
            with self.journal_path.open("r", encoding="utf-8") as stream:
                for line in stream:
                    if not line.strip():
                        continue
                    try:
                        value = json.loads(line)
                    except (json.JSONDecodeError, UnicodeError):
                        corrupt_lines += 1
                        continue
                    if isinstance(value, dict):
                        entries.append(value)
                    else:
                        corrupt_lines += 1
        except OSError:
            if self._logger is not None:
                self._logger.exception("No se pudo leer el journal existente")
            raise
        return entries, corrupt_lines

    def _load_state(self) -> dict[str, Any]:
        state = self._new_state()
        if not self.state_path.is_file():
            return state
        try:
            value = json.loads(self.state_path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError):
            if self._logger is not None:
                self._logger.warning(
                    "El estado del journal no era legible; se reconstruirá"
                )
            return state
        if not isinstance(value, Mapping) or value.get("schema_version") != SCHEMA_VERSION:
            return state

        for key in ("total_recorded", "throttled_progress"):
            candidate = value.get(key)
            if isinstance(candidate, int) and not isinstance(candidate, bool):
                state[key] = max(candidate, 0)
        created_at = value.get("created_at")
        if isinstance(created_at, str) and created_at:
            state["created_at"] = created_at
        last_export = value.get("last_export")
        if isinstance(last_export, Mapping):
            state["last_export"] = _json_safe(last_export, frozenset())
        return state

    def _new_state(self, *, created_at: str | None = None) -> dict[str, Any]:
        now = _utc_now()
        return {
            "schema_version": SCHEMA_VERSION,
            "created_at": created_at or now,
            "updated_at": now,
            "stored_entries": 0,
            "total_recorded": 0,
            "throttled_progress": 0,
            "corrupt_lines": 0,
            "event_counts": {},
            "last_event": None,
            "last_export": None,
        }

    def _reconcile_state(self, *, corrupt_lines: int) -> None:
        with self._lock:
            largest_sequence = max(
                (
                    entry.get("sequence", 0)
                    for entry in self._entries
                    if isinstance(entry.get("sequence"), int)
                ),
                default=0,
            )
            self._state["total_recorded"] = max(
                int(self._state["total_recorded"]),
                len(self._entries),
                largest_sequence,
            )
            self._state["corrupt_lines"] = corrupt_lines
            self._reconcile_retained_state_unlocked()

    def _reconcile_retained_state_unlocked(self) -> None:
        counts = Counter(
            str(entry.get("event", "unknown"))
            for entry in self._entries
        )
        self._state["stored_entries"] = len(self._entries)
        self._state["event_counts"] = dict(sorted(counts.items()))
        self._state["last_event"] = (
            {
                "sequence": self._entries[-1].get("sequence"),
                "event": self._entries[-1].get("event"),
                "event_id": self._entries[-1].get("event_id"),
                "recorded_at": self._entries[-1].get("recorded_at"),
            }
            if self._entries
            else None
        )

    def _write_state_unlocked(self) -> None:
        content = json.dumps(
            self._state,
            ensure_ascii=False,
            allow_nan=False,
            indent=2,
            sort_keys=True,
        )
        _atomic_write_text(self.state_path, f"{content}\n")

    def _throttle_progress(
        self,
        payload: Mapping[str, Any],
        progress_step: float,
        *,
        tracker_limit: int,
    ) -> bool:
        raw_progress = payload.get("progress")
        if (
            not isinstance(raw_progress, (int, float))
            or isinstance(raw_progress, bool)
            or not math.isfinite(float(raw_progress))
        ):
            return False
        progress = max(0.0, min(100.0, float(raw_progress)))
        identity = self._download_identity(payload)
        previous = self._last_download_progress.get(identity)
        if (
            previous is not None
            and progress >= previous
            and progress < 100
            and progress - previous < progress_step
        ):
            self._last_download_progress.move_to_end(identity)
            return True
        self._last_download_progress[identity] = progress
        self._last_download_progress.move_to_end(identity)
        while len(self._last_download_progress) > tracker_limit:
            self._last_download_progress.popitem(last=False)
        return False

    @staticmethod
    def _download_identity(payload: Mapping[str, Any]) -> str:
        for key in ("download_id", "job_id", "id"):
            value = payload.get(key)
            if isinstance(value, (str, int)) and not isinstance(value, bool):
                return str(value)
        return "<unknown>"

    def _ensure_open(self) -> None:
        if self._closed:
            raise RuntimeError("workflow journal is closed")

    def _make_directory_private(self, path: Path) -> None:
        try:
            path.chmod(0o700)
        except OSError as exc:
            if self._logger is not None:
                self._logger.warning(
                    "No se pudieron restringir permisos de %s: %s",
                    path,
                    exc,
                )

    def _make_file_private(self, path: Path) -> None:
        try:
            path.chmod(0o600)
        except OSError as exc:
            if self._logger is not None:
                self._logger.warning(
                    "No se pudieron restringir permisos de %s: %s",
                    path,
                    exc,
                )


__all__ = ["WorkflowJournal"]
