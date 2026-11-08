from __future__ import annotations

import re
from enum import StrEnum
from pathlib import Path
from typing import Any, Annotated, Literal
from urllib.parse import urlparse

from pydantic import BaseModel, ConfigDict, Field, field_validator

from .config import SUPPORTED_MEDIA_EXTENSIONS, SUPPORTED_PLAYLIST_EXTENSIONS


NonEmptyText = Annotated[str, Field(min_length=1, max_length=240)]


class StrictModel(BaseModel):
    model_config = ConfigDict(extra="forbid", str_strip_whitespace=True)


class MediaKind(StrEnum):
    AUDIO = "audio"
    VIDEO = "video"


class ImportMode(StrEnum):
    COPY = "copy"
    REFERENCE = "reference"


def _validated_youtube_url(value: str) -> str:
    parsed = urlparse(value.strip())
    host = (parsed.hostname or "").lower().removeprefix("www.")
    if parsed.scheme not in {"http", "https"}:
        raise ValueError("La URL debe usar http o https")
    if host not in {"youtube.com", "m.youtube.com", "music.youtube.com", "youtu.be"}:
        raise ValueError("Sólo se aceptan URLs de YouTube")
    return value.strip()


class DownloadRequest(StrictModel):
    url: Annotated[str, Field(min_length=8, max_length=2048)]
    type: MediaKind = MediaKind.AUDIO
    title: Annotated[str, Field(max_length=500)] = ""
    thumbnail: Annotated[str, Field(max_length=2048)] = ""

    _validate_url = field_validator("url")(_validated_youtube_url)

    @field_validator("thumbnail")
    @classmethod
    def validate_thumbnail(cls, value: str) -> str:
        if not value:
            return value
        parsed = urlparse(value)
        if parsed.scheme != "https":
            raise ValueError("La miniatura debe usar HTTPS")
        return value


class PlaylistDownloadRequest(StrictModel):
    url: Annotated[str, Field(min_length=8, max_length=2048)]
    type: MediaKind = MediaKind.AUDIO

    _validate_url = field_validator("url")(_validated_youtube_url)


class LibraryImportRequest(StrictModel):
    paths: Annotated[list[str], Field(min_length=1, max_length=512)]
    mode: ImportMode = ImportMode.COPY
    playlist_name: Annotated[str, Field(max_length=240)] = ""

    @field_validator("paths")
    @classmethod
    def validate_paths(cls, values: list[str]) -> list[str]:
        accepted = SUPPORTED_MEDIA_EXTENSIONS | SUPPORTED_PLAYLIST_EXTENSIONS
        cleaned: list[str] = []
        for raw in values:
            value = raw.strip()
            if not value or "\x00" in value:
                raise ValueError("Ruta inválida")
            path = Path(value).expanduser()
            if path.suffix.lower() not in accepted and not path.is_dir():
                raise ValueError(f"Formato no soportado: {path.suffix or path.name}")
            cleaned.append(str(path))
        return list(dict.fromkeys(cleaned))


class PlaylistCreateRequest(StrictModel):
    name: NonEmptyText

    @field_validator("name")
    @classmethod
    def validate_name(cls, value: str) -> str:
        if re.search(r"[/\\\x00-\x1f]", value):
            raise ValueError("El nombre contiene caracteres no permitidos")
        return value


class PlaylistTrackRequest(StrictModel):
    track_id: Annotated[str, Field(min_length=8, max_length=64)] = ""
    filename: Annotated[str, Field(max_length=500)] = ""

    @field_validator("filename")
    @classmethod
    def require_identifier(cls, value: str, info: Any) -> str:
        if not value and not info.data.get("track_id"):
            raise ValueError("Se requiere track_id o filename")
        return value


class LegacyPlaylistTrackRequest(PlaylistTrackRequest):
    playlist_name: NonEmptyText


class ProcessingRequest(StrictModel):
    track_id: Annotated[str, Field(max_length=64)] = ""
    filename: Annotated[str, Field(max_length=500)] = ""
    local_path: Annotated[str, Field(max_length=4096)] = ""
    title: Annotated[str, Field(max_length=500)] = ""


class SeparationRequest(ProcessingRequest):
    output_format: Literal["FLAC", "MP3", "WAV"] = "FLAC"


class FingerprintRequest(ProcessingRequest):
    intensity: Literal["balanced", "strong", "quality"] = "strong"


class ChipmunkRequest(ProcessingRequest):
    pitch_semitones: Annotated[float, Field(ge=1, le=12)] = 7
    speed: Annotated[float, Field(ge=0.5, le=2)] = 1
    brightness_db: Annotated[float, Field(ge=0, le=12)] = 3


class SettingsPatch(StrictModel):
    audio_quality: Literal["128", "192", "256", "320", "best"] | None = None
    video_quality: Literal[
        "best", "360", "480", "720", "1080", "1440", "2160", "4320"
    ] | None = None
    auto_thumbnail: bool | None = None
    max_workers: Annotated[int | None, Field(ge=1, le=32)] = None
    import_mode: Literal["copy", "reference"] | None = None
    default_download_type: MediaKind | None = None
    max_concurrent_downloads: Annotated[int | None, Field(ge=1, le=8)] = None
    max_concurrent_processing: Annotated[int | None, Field(ge=1, le=4)] = None
    youtube_cookie_file: Annotated[str | None, Field(max_length=4096)] = None
    youtube_cookies_browser: Annotated[str | None, Field(max_length=4096)] = None
    youtube_extractor_args: Annotated[str | None, Field(max_length=16384)] = None
    youtube_proxy: Annotated[str | None, Field(max_length=4096)] = None
    download_socket_timeout: Annotated[int | None, Field(ge=1, le=120)] = None
    download_retries: Annotated[int | None, Field(ge=0, le=10)] = None
    filename_template: Annotated[str | None, Field(min_length=1, max_length=200)] = None
    preserve_source_metadata: bool | None = None
    developer_mode: bool | None = None
    show_advanced_tools: bool | None = None
    scan_library_on_startup: bool | None = None
    plugin_auto_reload: bool | None = None
    allow_unsigned_plugins: bool | None = None
    download_subtitles: bool | None = None
    theme: Literal["system", "light", "dark"] | None = None
    language: Literal["es", "en"] | None = None


class PluginToggleRequest(StrictModel):
    enabled: bool


class PluginCommandRequest(StrictModel):
    arguments: dict[str, Any] = Field(default_factory=dict)


class PluginScaffoldRequest(StrictModel):
    plugin_id: Annotated[str, Field(pattern=r"^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$", max_length=80)]
    name: NonEmptyText
    description: Annotated[str, Field(max_length=500)] = ""
