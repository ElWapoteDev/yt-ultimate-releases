"""Versioned developer-plugin manifest parsing and path validation."""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from types import MappingProxyType
from typing import Any, Mapping

from .errors import PluginError, PluginErrorCode
from .validation import (
    ValidationIssue,
    ensure_json_value,
    schema_defaults,
    validate_config,
    validate_config_schema,
)


MANIFEST_FILENAME = "plugin.json"
MANIFEST_VERSION = 1
SDK_API_VERSION = 1
MAX_MANIFEST_BYTES = 256 * 1024

_PLUGIN_ID_RE = re.compile(
    r"^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$",
    re.ASCII,
)
_SEMVER_RE = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
    r"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$"
)
_PERMISSION_RE = re.compile(
    r"^[a-z][a-z0-9_-]*(?:\.[a-z][a-z0-9_-]*)+$",
    re.ASCII,
)
_KNOWN_FIELDS = {
    "manifest_version",
    "id",
    "name",
    "version",
    "api_version",
    "entrypoint",
    "description",
    "author",
    "license",
    "homepage",
    "permissions",
    "default_config",
    "config_schema",
}


@dataclass(frozen=True, slots=True)
class PluginManifest:
    manifest_version: int
    id: str
    name: str
    version: str
    api_version: int
    entrypoint: str
    description: str
    author: str
    license: str
    homepage: str
    permissions: tuple[str, ...]
    default_config: Mapping[str, Any]
    config_schema: Mapping[str, Any]

    @classmethod
    def from_file(cls, path: Path | str) -> "PluginManifest":
        manifest_path = Path(path)
        try:
            size = manifest_path.stat().st_size
        except OSError as exc:
            raise PluginError(
                PluginErrorCode.INVALID_MANIFEST,
                "plugin manifest is not readable",
                details={"file": manifest_path.name},
                cause=exc,
            ) from exc
        if size > MAX_MANIFEST_BYTES:
            raise PluginError(
                PluginErrorCode.INVALID_MANIFEST,
                "plugin manifest is too large",
                details={"max_bytes": MAX_MANIFEST_BYTES},
            )
        try:
            raw = json.loads(manifest_path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError) as exc:
            raise PluginError(
                PluginErrorCode.INVALID_MANIFEST,
                "plugin manifest must contain valid UTF-8 JSON",
                details={"file": manifest_path.name},
                cause=exc,
            ) from exc
        return cls.from_dict(raw)

    @classmethod
    def from_dict(cls, raw: Any) -> "PluginManifest":
        if not isinstance(raw, Mapping):
            raise _manifest_error("manifest root must be an object")

        unknown = [
            key
            for key in raw
            if key not in _KNOWN_FIELDS
            and not (isinstance(key, str) and key.startswith("x-"))
        ]
        if unknown:
            raise _manifest_error(
                "manifest contains unknown fields",
                fields=sorted(str(key) for key in unknown),
            )

        manifest_version = _required_int(raw, "manifest_version")
        if manifest_version != MANIFEST_VERSION:
            raise PluginError(
                PluginErrorCode.UNSUPPORTED_MANIFEST_VERSION,
                f"manifest version {manifest_version} is not supported",
                details={"supported": [MANIFEST_VERSION]},
            )

        plugin_id = _required_string(raw, "id", max_length=96)
        validate_plugin_id(plugin_id)
        name = _required_string(raw, "name", max_length=120)
        version = _required_string(raw, "version", max_length=80)
        if not _SEMVER_RE.fullmatch(version):
            raise _manifest_error(
                "version must follow semantic versioning (for example 1.2.0)",
                plugin_id=plugin_id,
                field="version",
            )

        api_version = _required_int(raw, "api_version")
        if api_version != SDK_API_VERSION:
            raise PluginError(
                PluginErrorCode.UNSUPPORTED_API_VERSION,
                f"plugin API version {api_version} is not supported",
                plugin_id=plugin_id,
                details={"supported": [SDK_API_VERSION]},
            )

        entrypoint = _required_string(raw, "entrypoint", max_length=240)
        validate_entrypoint(entrypoint, plugin_id=plugin_id)

        permissions_raw = raw.get("permissions", [])
        if not isinstance(permissions_raw, list) or any(
            not isinstance(item, str) for item in permissions_raw
        ):
            raise _manifest_error(
                "permissions must be an array of strings",
                plugin_id=plugin_id,
                field="permissions",
            )
        if len(set(permissions_raw)) != len(permissions_raw):
            raise _manifest_error(
                "permissions must not contain duplicates",
                plugin_id=plugin_id,
                field="permissions",
            )
        for permission in permissions_raw:
            if len(permission) > 96 or not _PERMISSION_RE.fullmatch(permission):
                raise _manifest_error(
                    f"invalid permission identifier {permission!r}",
                    plugin_id=plugin_id,
                    field="permissions",
                )

        default_config = raw.get("default_config", {})
        if not isinstance(default_config, Mapping):
            raise _manifest_error(
                "default_config must be an object",
                plugin_id=plugin_id,
                field="default_config",
            )
        config_schema = raw.get(
            "config_schema",
            {"type": "object", "additionalProperties": True},
        )
        if not isinstance(config_schema, Mapping):
            raise _manifest_error(
                "config_schema must be an object",
                plugin_id=plugin_id,
                field="config_schema",
            )
        try:
            ensure_json_value(default_config, "$.default_config")
            validate_config_schema(config_schema)
            combined_defaults = schema_defaults(config_schema)
            combined_defaults.update(_clone_json(default_config))
            validate_config(combined_defaults, config_schema)
        except ValidationIssue as exc:
            raise _manifest_error(
                f"invalid configuration declaration: {exc}",
                plugin_id=plugin_id,
                field="config_schema",
            ) from exc

        return cls(
            manifest_version=manifest_version,
            id=plugin_id,
            name=name,
            version=version,
            api_version=api_version,
            entrypoint=entrypoint,
            description=_optional_string(raw, "description", max_length=1_000),
            author=_optional_string(raw, "author", max_length=200),
            license=_optional_string(raw, "license", max_length=80),
            homepage=_optional_string(raw, "homepage", max_length=500),
            permissions=tuple(permissions_raw),
            default_config=_freeze_json(combined_defaults),
            config_schema=_freeze_json(config_schema),
        )

    @property
    def entrypoint_parts(self) -> tuple[PurePosixPath, str]:
        file_name, callable_name = self.entrypoint.rsplit(":", 1)
        return PurePosixPath(file_name), callable_name

    def resolve_entrypoint(self, plugin_root: Path | str) -> tuple[Path, str]:
        """Resolve and verify the entrypoint without importing it."""

        root = Path(plugin_root)
        try:
            resolved_root = root.resolve(strict=True)
        except OSError as exc:
            raise PluginError(
                PluginErrorCode.INVALID_ENTRYPOINT,
                "plugin directory is not accessible",
                plugin_id=self.id,
                cause=exc,
            ) from exc
        relative_file, callable_name = self.entrypoint_parts
        candidate = resolved_root.joinpath(*relative_file.parts)
        if candidate.is_symlink():
            raise PluginError(
                PluginErrorCode.PATH_ESCAPE,
                "symlink entrypoints are not allowed",
                plugin_id=self.id,
            )
        try:
            resolved_file = candidate.resolve(strict=True)
            resolved_file.relative_to(resolved_root)
        except (OSError, ValueError) as exc:
            raise PluginError(
                PluginErrorCode.INVALID_ENTRYPOINT,
                "entrypoint must be a regular .py file inside the plugin directory",
                plugin_id=self.id,
                details={"entrypoint": self.entrypoint},
                cause=exc,
            ) from exc
        if not resolved_file.is_file() or resolved_file.suffix != ".py":
            raise PluginError(
                PluginErrorCode.INVALID_ENTRYPOINT,
                "entrypoint must be a regular .py file",
                plugin_id=self.id,
                details={"entrypoint": self.entrypoint},
            )
        return resolved_file, callable_name

    def to_dict(self) -> dict[str, Any]:
        return {
            "manifest_version": self.manifest_version,
            "id": self.id,
            "name": self.name,
            "version": self.version,
            "api_version": self.api_version,
            "entrypoint": self.entrypoint,
            "description": self.description,
            "author": self.author,
            "license": self.license,
            "homepage": self.homepage,
            "permissions": list(self.permissions),
            "default_config": _clone_json(self.default_config),
            "config_schema": _clone_json(self.config_schema),
        }


def validate_plugin_id(plugin_id: str) -> str:
    if (
        not isinstance(plugin_id, str)
        or not 1 <= len(plugin_id) <= 96
        or not _PLUGIN_ID_RE.fullmatch(plugin_id)
    ):
        raise PluginError(
            PluginErrorCode.INVALID_PLUGIN_ID,
            "plugin ID must be lowercase, start with a letter, and use only "
            "letters, numbers, '.', '_' or '-'",
            plugin_id=plugin_id if isinstance(plugin_id, str) else None,
        )
    return plugin_id


def validate_entrypoint(entrypoint: str, *, plugin_id: str | None = None) -> str:
    if entrypoint.count(":") != 1:
        raise PluginError(
            PluginErrorCode.INVALID_ENTRYPOINT,
            "entrypoint must use the format relative/file.py:callable",
            plugin_id=plugin_id,
        )
    file_name, callable_name = entrypoint.split(":", 1)
    if "\\" in file_name:
        raise PluginError(
            PluginErrorCode.INVALID_ENTRYPOINT,
            "entrypoint paths must use '/' separators",
            plugin_id=plugin_id,
        )
    relative_file = PurePosixPath(file_name)
    if (
        not file_name
        or relative_file.is_absolute()
        or ".." in relative_file.parts
        or "." in relative_file.parts
        or relative_file.suffix != ".py"
        or any(not part for part in relative_file.parts)
    ):
        raise PluginError(
            PluginErrorCode.INVALID_ENTRYPOINT,
            "entrypoint file must be a relative .py path without traversal",
            plugin_id=plugin_id,
        )
    if not callable_name.isidentifier():
        raise PluginError(
            PluginErrorCode.INVALID_ENTRYPOINT,
            "entrypoint callable must be a valid Python identifier",
            plugin_id=plugin_id,
        )
    return entrypoint


def merge_plugin_config(
    manifest: PluginManifest,
    overrides: Mapping[str, Any] | None,
) -> dict[str, Any]:
    """Merge user overrides over defaults, then validate the complete result."""

    if overrides is None:
        overrides = {}
    if not isinstance(overrides, Mapping):
        raise PluginError(
            PluginErrorCode.CONFIG_INVALID,
            "plugin configuration must be an object",
            plugin_id=manifest.id,
        )
    try:
        ensure_json_value(overrides, "$")
    except ValidationIssue as exc:
        raise PluginError(
            PluginErrorCode.CONFIG_INVALID,
            f"invalid plugin configuration: {exc}",
            plugin_id=manifest.id,
            details={"path": exc.path, "reason": exc.reason},
        ) from exc
    merged = _deep_merge(
        _clone_json(manifest.default_config),
        _clone_json(overrides),
    )
    try:
        validate_config(merged, manifest.config_schema)
    except ValidationIssue as exc:
        raise PluginError(
            PluginErrorCode.CONFIG_INVALID,
            f"invalid plugin configuration: {exc}",
            plugin_id=manifest.id,
            details={"path": exc.path, "reason": exc.reason},
        ) from exc
    return merged


def _required_string(
    raw: Mapping[str, Any],
    field: str,
    *,
    max_length: int,
) -> str:
    value = raw.get(field)
    if not isinstance(value, str) or not value.strip():
        raise _manifest_error(f"{field} must be a non-empty string", field=field)
    value = value.strip()
    if len(value) > max_length:
        raise _manifest_error(
            f"{field} must not exceed {max_length} characters",
            field=field,
        )
    return value


def _optional_string(
    raw: Mapping[str, Any],
    field: str,
    *,
    max_length: int,
) -> str:
    value = raw.get(field, "")
    if not isinstance(value, str):
        raise _manifest_error(f"{field} must be a string", field=field)
    value = value.strip()
    if len(value) > max_length:
        raise _manifest_error(
            f"{field} must not exceed {max_length} characters",
            field=field,
        )
    return value


def _required_int(raw: Mapping[str, Any], field: str) -> int:
    value = raw.get(field)
    if not isinstance(value, int) or isinstance(value, bool):
        raise _manifest_error(f"{field} must be an integer", field=field)
    return value


def _manifest_error(
    message: str,
    *,
    plugin_id: str | None = None,
    **details: Any,
) -> PluginError:
    return PluginError(
        PluginErrorCode.INVALID_MANIFEST,
        message,
        plugin_id=plugin_id,
        details=details,
    )


def _deep_merge(base: dict[str, Any], overrides: Mapping[str, Any]) -> dict[str, Any]:
    for key, value in overrides.items():
        if (
            key in base
            and isinstance(base[key], dict)
            and isinstance(value, Mapping)
        ):
            base[key] = _deep_merge(base[key], value)
        else:
            base[key] = _clone_json(value)
    return base


def _clone_json(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {str(key): _clone_json(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_clone_json(item) for item in value]
    return value


def _freeze_json(value: Any) -> Any:
    if isinstance(value, Mapping):
        return MappingProxyType(
            {str(key): _freeze_json(item) for key, item in value.items()}
        )
    if isinstance(value, list):
        return tuple(_freeze_json(item) for item in value)
    return value


__all__ = [
    "MANIFEST_FILENAME",
    "MANIFEST_VERSION",
    "MAX_MANIFEST_BYTES",
    "SDK_API_VERSION",
    "PluginManifest",
    "merge_plugin_config",
    "validate_entrypoint",
    "validate_plugin_id",
]
