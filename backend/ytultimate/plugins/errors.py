"""Structured errors exposed by the plugin runtime."""

from __future__ import annotations

from enum import Enum
from typing import Any, Mapping


class PluginErrorCode(str, Enum):
    INVALID_MANIFEST = "invalid_manifest"
    UNSUPPORTED_MANIFEST_VERSION = "unsupported_manifest_version"
    UNSUPPORTED_API_VERSION = "unsupported_api_version"
    INVALID_PLUGIN_ID = "invalid_plugin_id"
    INVALID_ENTRYPOINT = "invalid_entrypoint"
    PATH_ESCAPE = "path_escape"
    PLUGIN_NOT_FOUND = "plugin_not_found"
    DUPLICATE_PLUGIN_ID = "duplicate_plugin_id"
    DEVELOPER_MODE_REQUIRED = "developer_mode_required"
    PLUGIN_DISABLED = "plugin_disabled"
    PLUGIN_ALREADY_LOADED = "plugin_already_loaded"
    LOAD_TIMEOUT = "load_timeout"
    LOAD_FAILED = "load_failed"
    SETUP_FAILED = "setup_failed"
    COMMAND_NOT_FOUND = "command_not_found"
    COMMAND_CONFLICT = "command_conflict"
    COMMAND_TIMEOUT = "command_timeout"
    COMMAND_FAILED = "command_failed"
    PERMISSION_NOT_DECLARED = "permission_not_declared"
    CONFIG_INVALID = "config_invalid"
    STATE_IO = "state_io"
    TEARDOWN_FAILED = "teardown_failed"
    SCAFFOLD_EXISTS = "scaffold_exists"
    SCAFFOLD_FAILED = "scaffold_failed"
    INTERNAL = "internal"


class PluginError(RuntimeError):
    """A safe-to-serialize runtime error.

    ``cause`` is kept for logs/exception chaining but deliberately omitted from
    :meth:`to_dict` so internal tracebacks and paths do not leak to API clients.
    """

    def __init__(
        self,
        code: PluginErrorCode | str,
        message: str,
        *,
        plugin_id: str | None = None,
        details: Mapping[str, Any] | None = None,
        recoverable: bool = True,
        cause: BaseException | None = None,
    ) -> None:
        self.code = (
            code if isinstance(code, PluginErrorCode) else PluginErrorCode(code)
        )
        self.message = message
        self.plugin_id = plugin_id
        self.details = dict(details or {})
        self.recoverable = recoverable
        self.cause = cause
        super().__init__(message)

    def to_dict(self) -> dict[str, Any]:
        result: dict[str, Any] = {
            "code": self.code.value,
            "message": self.message,
            "recoverable": self.recoverable,
        }
        if self.plugin_id is not None:
            result["plugin_id"] = self.plugin_id
        if self.details:
            result["details"] = dict(self.details)
        return result


__all__ = ["PluginError", "PluginErrorCode"]
