"""Plugin enable/config state with an atomic JSON fallback."""

from __future__ import annotations

import json
import math
import os
import tempfile
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Mapping, Protocol, runtime_checkable

from .errors import PluginError, PluginErrorCode
from .manifest import validate_plugin_id
from .validation import ValidationIssue, ensure_json_value


STATE_FILE_VERSION = 1
MAX_STATE_BYTES = 2 * 1024 * 1024


@dataclass(frozen=True, slots=True)
class PluginState:
    """Persisted user choices. Plugins are disabled unless explicitly enabled."""

    enabled: bool = False
    config: Mapping[str, Any] = field(default_factory=dict)
    updated_at: float = 0.0


@runtime_checkable
class StateStore(Protocol):
    """Minimal adapter contract for SQLite, preferences, or another store."""

    def load(self, plugin_id: str) -> PluginState:
        """Return state, or the disabled/default state when no record exists."""
        ...

    def save(self, plugin_id: str, state: PluginState) -> None:
        """Atomically persist one plugin state."""
        ...


class MemoryStateStore:
    """Small in-memory implementation useful for tests and ephemeral sessions."""

    def __init__(self) -> None:
        self._states: dict[str, PluginState] = {}
        self._lock = threading.RLock()

    def load(self, plugin_id: str) -> PluginState:
        validate_plugin_id(plugin_id)
        with self._lock:
            return _clone_state(self._states.get(plugin_id, PluginState()))

    def save(self, plugin_id: str, state: PluginState) -> None:
        validate_plugin_id(plugin_id)
        _validate_state(plugin_id, state)
        with self._lock:
            self._states[plugin_id] = _clone_state(state)


class JsonStateStore:
    """Dependency-free state store using same-directory atomic replacement."""

    def __init__(self, path: Path | str) -> None:
        self.path = Path(path)
        self._lock = threading.RLock()

    def load(self, plugin_id: str) -> PluginState:
        validate_plugin_id(plugin_id)
        with self._lock:
            document = self._read_document()
            raw = document["plugins"].get(plugin_id)
            if raw is None:
                return PluginState()
            return self._decode_state(plugin_id, raw)

    def save(self, plugin_id: str, state: PluginState) -> None:
        validate_plugin_id(plugin_id)
        _validate_state(plugin_id, state)
        with self._lock:
            document = self._read_document()
            document["plugins"][plugin_id] = {
                "enabled": state.enabled,
                "config": _clone_json(state.config),
                "updated_at": state.updated_at or time.time(),
            }
            self._write_document(document)

    def all(self) -> dict[str, PluginState]:
        """Return a defensive copy of all records (mainly for diagnostics)."""

        with self._lock:
            document = self._read_document()
            return {
                plugin_id: self._decode_state(plugin_id, raw)
                for plugin_id, raw in document["plugins"].items()
            }

    def _read_document(self) -> dict[str, Any]:
        if not self.path.exists():
            return {"version": STATE_FILE_VERSION, "plugins": {}}
        if self.path.is_symlink():
            raise PluginError(
                PluginErrorCode.STATE_IO,
                "plugin state file cannot be a symlink",
            )
        try:
            if self.path.stat().st_size > MAX_STATE_BYTES:
                raise PluginError(
                    PluginErrorCode.STATE_IO,
                    "plugin state file is too large",
                    details={"max_bytes": MAX_STATE_BYTES},
                )
            raw = json.loads(self.path.read_text(encoding="utf-8"))
        except PluginError:
            raise
        except (OSError, UnicodeError, json.JSONDecodeError) as exc:
            raise PluginError(
                PluginErrorCode.STATE_IO,
                "plugin state file is unreadable or corrupt",
                cause=exc,
            ) from exc
        if not isinstance(raw, dict):
            raise PluginError(
                PluginErrorCode.STATE_IO,
                "plugin state root must be an object",
            )
        if raw.get("version") != STATE_FILE_VERSION:
            raise PluginError(
                PluginErrorCode.STATE_IO,
                "unsupported plugin state version",
                details={
                    "found": raw.get("version"),
                    "supported": STATE_FILE_VERSION,
                },
            )
        if not isinstance(raw.get("plugins"), dict):
            raise PluginError(
                PluginErrorCode.STATE_IO,
                "plugin state must contain a plugins object",
            )
        return raw

    def _write_document(self, document: Mapping[str, Any]) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        if self.path.exists() and self.path.is_symlink():
            raise PluginError(
                PluginErrorCode.STATE_IO,
                "plugin state file cannot be a symlink",
            )
        temporary_path: Path | None = None
        try:
            descriptor, raw_path = tempfile.mkstemp(
                prefix=f".{self.path.name}.",
                suffix=".tmp",
                dir=self.path.parent,
            )
            temporary_path = Path(raw_path)
            with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
                json.dump(
                    document,
                    stream,
                    ensure_ascii=False,
                    indent=2,
                    sort_keys=True,
                    allow_nan=False,
                )
                stream.write("\n")
                stream.flush()
                os.fsync(stream.fileno())
            os.chmod(temporary_path, 0o600)
            os.replace(temporary_path, self.path)
            temporary_path = None
        except (OSError, TypeError, ValueError) as exc:
            raise PluginError(
                PluginErrorCode.STATE_IO,
                "could not persist plugin state",
                cause=exc,
            ) from exc
        finally:
            if temporary_path is not None:
                try:
                    temporary_path.unlink(missing_ok=True)
                except OSError:
                    pass

    @staticmethod
    def _decode_state(plugin_id: str, raw: Any) -> PluginState:
        if not isinstance(raw, Mapping):
            raise PluginError(
                PluginErrorCode.STATE_IO,
                "invalid plugin state record",
                plugin_id=plugin_id,
            )
        enabled = raw.get("enabled", False)
        config = raw.get("config", {})
        updated_at = raw.get("updated_at", 0.0)
        state = PluginState(
            enabled=enabled,
            config=config,
            updated_at=updated_at,
        )
        _validate_state(plugin_id, state)
        return _clone_state(state)


def _validate_state(plugin_id: str, state: PluginState) -> None:
    if not isinstance(state, PluginState):
        raise TypeError("state must be a PluginState")
    if not isinstance(state.enabled, bool):
        raise PluginError(
            PluginErrorCode.STATE_IO,
            "enabled state must be a boolean",
            plugin_id=plugin_id,
        )
    if not isinstance(state.config, Mapping):
        raise PluginError(
            PluginErrorCode.STATE_IO,
            "plugin configuration state must be an object",
            plugin_id=plugin_id,
        )
    if (
        not isinstance(state.updated_at, (int, float))
        or isinstance(state.updated_at, bool)
        or not math.isfinite(state.updated_at)
        or state.updated_at < 0
    ):
        raise PluginError(
            PluginErrorCode.STATE_IO,
            "updated_at must be a non-negative number",
            plugin_id=plugin_id,
        )
    try:
        ensure_json_value(state.config, "$.config")
    except ValidationIssue as exc:
        raise PluginError(
            PluginErrorCode.STATE_IO,
            f"plugin state is not JSON-compatible: {exc}",
            plugin_id=plugin_id,
        ) from exc


def _clone_state(state: PluginState) -> PluginState:
    return PluginState(
        enabled=state.enabled,
        config=_clone_json(state.config),
        updated_at=float(state.updated_at),
    )


def _clone_json(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {str(key): _clone_json(item) for key, item in value.items()}
    if isinstance(value, list):
        return [_clone_json(item) for item in value]
    return value


__all__ = [
    "JsonStateStore",
    "MemoryStateStore",
    "PluginState",
    "STATE_FILE_VERSION",
    "StateStore",
]
