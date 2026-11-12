"""Typed application preferences backed by SQLite."""

from __future__ import annotations

import os
from collections.abc import Mapping
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from ..database import Database

if TYPE_CHECKING:
    from collections.abc import Iterable

    from ..config import AppConfig


_UNSET = object()


@dataclass(frozen=True, slots=True)
class SettingSpec:
    """Description and validation constraints for one preference."""

    key: str
    default: Any
    value_type: type
    description: str
    choices: tuple[Any, ...] = ()
    minimum: int | float | None = None
    maximum: int | float | None = None
    minimum_length: int | None = None
    maximum_length: int | None = None
    advanced: bool = False

    def validate(self, value: Any) -> Any:
        if self.value_type is bool:
            if type(value) is not bool:
                raise TypeError(f"{self.key} must be a boolean")
        elif self.value_type is int:
            if type(value) is not int:
                raise TypeError(f"{self.key} must be an integer")
        elif not isinstance(value, self.value_type):
            raise TypeError(
                f"{self.key} must be {self.value_type.__name__}"
            )

        if self.choices and value not in self.choices:
            options = ", ".join(repr(item) for item in self.choices)
            raise ValueError(f"{self.key} must be one of: {options}")
        if self.minimum is not None and value < self.minimum:
            raise ValueError(f"{self.key} must be at least {self.minimum}")
        if self.maximum is not None and value > self.maximum:
            raise ValueError(f"{self.key} must be at most {self.maximum}")
        if self.minimum_length is not None and len(value) < self.minimum_length:
            raise ValueError(
                f"{self.key} must contain at least "
                f"{self.minimum_length} characters"
            )
        if self.maximum_length is not None and len(value) > self.maximum_length:
            raise ValueError(
                f"{self.key} cannot exceed {self.maximum_length} characters"
            )
        return value

    def as_dict(self) -> dict[str, Any]:
        type_name = {
            bool: "boolean",
            int: "integer",
            str: "string",
            float: "number",
        }.get(self.value_type, self.value_type.__name__)
        return {
            "key": self.key,
            "type": type_name,
            "default": self.default,
            "description": self.description,
            "choices": list(self.choices),
            "minimum": self.minimum,
            "maximum": self.maximum,
            "minimum_length": self.minimum_length,
            "maximum_length": self.maximum_length,
            "advanced": self.advanced,
        }


def _default_worker_count() -> int:
    return max(1, min(4, os.cpu_count() or 2))


CORE_SETTING_SPECS: tuple[SettingSpec, ...] = (
    SettingSpec(
        "audio_quality",
        "192",
        str,
        "Bitrate used for audio downloads.",
        ("128", "192", "256", "320", "best"),
    ),
    SettingSpec(
        "video_quality",
        "best",
        str,
        "Maximum resolution used for video downloads.",
        ("best", "4320", "2160", "1440", "1080", "720", "480", "360"),
    ),
    SettingSpec(
        "auto_thumbnail",
        True,
        bool,
        "Download and associate artwork automatically.",
    ),
    SettingSpec(
        "developer_mode",
        False,
        bool,
        "Expose plugin tools, diagnostics, and advanced configuration.",
        advanced=True,
    ),
    SettingSpec(
        "max_workers",
        _default_worker_count(),
        int,
        "Maximum number of concurrent background workers.",
        minimum=1,
        maximum=32,
        advanced=True,
    ),
    SettingSpec(
        "import_mode",
        "copy",
        str,
        "Default behavior when files are dropped into the application.",
        ("copy", "reference"),
    ),
    SettingSpec(
        "default_download_type",
        "audio",
        str,
        "Media type preselected for new downloads.",
        ("audio", "video"),
    ),
    SettingSpec(
        "max_concurrent_downloads",
        3,
        int,
        "Maximum number of simultaneous download jobs.",
        minimum=1,
        maximum=8,
        advanced=True,
    ),
    SettingSpec(
        "max_concurrent_processing",
        1,
        int,
        "Maximum number of simultaneous local processing jobs.",
        minimum=1,
        maximum=4,
        advanced=True,
    ),
    SettingSpec(
        "youtube_cookie_file", "", str,
        "Archivo Netscape de cookies elegido para descargas y streaming.",
        maximum_length=4096, advanced=True,
    ),
    SettingSpec(
        "youtube_cookies_browser", "", str,
        "Navegador autorizado como fuente de cookies; vacío usa sesión anónima.",
        maximum_length=4096, advanced=True,
    ),
    SettingSpec(
        "youtube_extractor_args", "", str,
        "Argumentos JSON de yt-dlp para clientes y proveedores de PO Token.",
        maximum_length=16384, advanced=True,
    ),
    SettingSpec(
        "youtube_proxy", "", str,
        "Proxy opcional compartido por extracción, descarga y streaming.",
        maximum_length=4096, advanced=True,
    ),
    SettingSpec(
        "download_socket_timeout", 20, int,
        "Tiempo máximo de espera por operación de red, en segundos.",
        minimum=1, maximum=120, advanced=True,
    ),
    SettingSpec(
        "download_retries", 2, int,
        "Reintentos de transporte ante fallos temporales.",
        minimum=0, maximum=10, advanced=True,
    ),
    SettingSpec(
        "filename_template",
        "{title}",
        str,
        "Template used to name newly downloaded media.",
        minimum_length=1,
        maximum_length=200,
        advanced=True,
    ),
    SettingSpec(
        "preserve_source_metadata",
        True,
        bool,
        "Preserve source title, channel, and artwork metadata.",
        advanced=True,
    ),
    SettingSpec(
        "scan_library_on_startup",
        True,
        bool,
        "Reconcile the managed library when the application starts.",
        advanced=True,
    ),
    SettingSpec(
        "plugin_auto_reload",
        False,
        bool,
        "Reload local plugins after their files change.",
        advanced=True,
    ),
    SettingSpec(
        "show_advanced_tools",
        False,
        bool,
        "Show processing and developer-oriented tools in the main interface.",
        advanced=True,
    ),
    SettingSpec(
        "allow_unsigned_plugins",
        False,
        bool,
        "Allow manually installed plugins that do not declare integrity data.",
        advanced=True,
    ),
    SettingSpec(
        "download_subtitles",
        False,
        bool,
        "Download subtitles when a video provides them.",
        advanced=True,
    ),
    SettingSpec(
        "theme",
        "system",
        str,
        "Application color theme.",
        ("system", "light", "dark"),
    ),
    SettingSpec(
        "language",
        "es",
        str,
        "Interface language.",
        ("es", "en"),
    ),
)


class SettingsService:
    """Validate and persist user-facing settings.

    Runtime values from :class:`AppConfig` remain outside this service.  The
    optional developer-mode environment override wins over the saved preference
    without destroying what the user previously selected.
    """

    DEFAULTS = {spec.key: spec.default for spec in CORE_SETTING_SPECS}

    def __init__(
        self,
        config: AppConfig,
        database: Database | None = None,
        *,
        extra_specs: Iterable[SettingSpec] = (),
    ) -> None:
        self.config = config
        self.database = database or Database(config)
        self._owns_database = database is None
        self._specs = {spec.key: spec for spec in CORE_SETTING_SPECS}
        for spec in extra_specs:
            self.register_spec(spec)

    def register_spec(self, spec: SettingSpec) -> None:
        if not spec.key or spec.key.strip() != spec.key:
            raise ValueError("Setting keys cannot be blank or padded")
        if spec.key in self._specs:
            raise ValueError(f"Setting spec already exists: {spec.key}")
        spec.validate(spec.default)
        self._specs[spec.key] = spec

    def _effective_value(self, key: str, value: Any) -> Any:
        if key == "developer_mode":
            override = getattr(
                self.config,
                "developer_mode_override",
                None,
            )
            if override is not None:
                return bool(override)
        return value

    def get_all(self) -> dict[str, Any]:
        saved = self.database.get_settings()
        result: dict[str, Any] = {}
        for key, spec in self._specs.items():
            value = saved.get(key, spec.default)
            try:
                value = spec.validate(value)
            except (TypeError, ValueError):
                value = spec.default
            result[key] = self._effective_value(key, value)
        return result

    def get(self, key: str | None = None, default: Any = _UNSET) -> Any:
        if key is None:
            return self.get_all()
        spec = self._specs.get(key)
        if spec is None:
            if default is not _UNSET:
                return default
            raise KeyError(f"Unknown setting: {key}")
        return self.get_all()[key]

    def update(self, patch: Mapping[str, Any]) -> dict[str, Any]:
        if not isinstance(patch, Mapping):
            raise TypeError("Settings update must be a mapping")
        validated: dict[str, Any] = {}
        for key, value in patch.items():
            spec = self._specs.get(key)
            if spec is None:
                raise KeyError(f"Unknown setting: {key}")
            validated[key] = spec.validate(value)
        if any(
            key.startswith("youtube_") or key in {"download_retries", "download_socket_timeout"}
            for key in validated
        ):
            from .youtube import YoutubeAccessPolicy

            YoutubeAccessPolicy(self.config, {**self.get_all(), **validated}).options()
        self.database.set_settings(validated)
        return self.get_all()

    def set(
        self,
        key_or_values: str | Mapping[str, Any],
        value: Any = _UNSET,
    ) -> dict[str, Any]:
        if isinstance(key_or_values, str):
            if value is _UNSET:
                raise TypeError("A value is required when setting one key")
            return self.update({key_or_values: value})
        if value is not _UNSET:
            raise TypeError("Do not pass value with a settings mapping")
        return self.update(key_or_values)

    def reset(
        self,
        keys: Iterable[str] | None = None,
    ) -> dict[str, Any]:
        selected = list(self._specs if keys is None else keys)
        unknown = [key for key in selected if key not in self._specs]
        if unknown:
            raise KeyError(f"Unknown setting: {unknown[0]}")
        with self.database.transaction():
            for key in selected:
                self.database.delete_setting(key)
        return self.get_all()

    def describe(self) -> list[dict[str, Any]]:
        values = self.get_all()
        override = getattr(self.config, "developer_mode_override", None)
        described: list[dict[str, Any]] = []
        for spec in self._specs.values():
            item = spec.as_dict()
            item["value"] = values[spec.key]
            item["overridden"] = (
                spec.key == "developer_mode" and override is not None
            )
            described.append(item)
        return described

    def close(self) -> None:
        if self._owns_database:
            self.database.close()
