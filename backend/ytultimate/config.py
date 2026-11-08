from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

from platformdirs import user_cache_path, user_data_path, user_log_path


APP_NAME = "YT Ultimate"
APP_AUTHOR = "YT Ultimate"
ENV_PREFIX = "YTU_"
DEFAULT_HOST = "127.0.0.1"
DEFAULT_PORT = 8000
SUPPORTED_MEDIA_EXTENSIONS = frozenset(
    {".mp3", ".mp4", ".m4a", ".webm", ".mkv", ".wav", ".flac", ".aac", ".ogg", ".opus"}
)
SUPPORTED_AUDIO_EXTENSIONS = frozenset(
    {".mp3", ".m4a", ".wav", ".flac", ".aac", ".ogg", ".opus"}
)
SUPPORTED_PLAYLIST_EXTENSIONS = frozenset({".m3u", ".m3u8"})
DEFAULT_SEPARATOR_MODEL = "model_bs_roformer_ep_317_sdr_12.9755.ckpt"
DEFAULT_SEPARATOR_MODEL_LABEL = "BS-RoFormer viperx SDR 12.9755"
# Console tools must never flash a window when the backend runs hidden on Windows.
HIDDEN_PROCESS_OPTIONS: dict[str, int] = (
    {"creationflags": subprocess.CREATE_NO_WINDOW} if os.name == "nt" else {}
)


def _env_path(name: str, fallback: Path) -> Path:
    raw = os.getenv(f"{ENV_PREFIX}{name}", "").strip()
    return Path(raw).expanduser().resolve() if raw else fallback.expanduser().resolve()


def _env_bool(name: str, fallback: bool = False) -> bool:
    raw = os.getenv(f"{ENV_PREFIX}{name}")
    if raw is None:
        return fallback
    return raw.strip().lower() in {"1", "true", "yes", "on"}


def _env_arguments(name: str) -> tuple[str, ...]:
    raw = os.getenv(f"{ENV_PREFIX}{name}", "").strip()
    if not raw:
        return ()
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise ValueError(f"{ENV_PREFIX}{name} must be a JSON array of strings") from exc
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise ValueError(f"{ENV_PREFIX}{name} must be a JSON array of strings")
    return tuple(value)


def _executable(name: str, env_name: str, resource_bin: Path) -> str:
    configured = os.getenv(f"{ENV_PREFIX}{env_name}", "").strip()
    if configured:
        # A venv interpreter is often a symlink. Resolving it launches the base
        # Python instead and loses the environment's audio dependencies.
        return str(Path(configured).expanduser().absolute())
    filenames = (name, f"{name}.exe") if os.name == "nt" else (name,)
    for directory in (resource_bin, Path(sys.executable).parent):
        for filename in filenames:
            candidate = directory / filename
            if candidate.is_file():
                return str(candidate)
    return shutil.which(name) or ""


def _separator_command(resource_bin: Path) -> tuple[str, tuple[str, ...]]:
    executable = _executable("audio-separator", "SEPARATOR_PATH", resource_bin)
    arguments = _env_arguments("SEPARATOR_ARGS")
    if (os.getenv(f"{ENV_PREFIX}SEPARATOR_PATH", "").strip() or arguments
            or getattr(sys, "frozen", False)):
        return executable, arguments
    if importlib.util.find_spec("audio_separator") is not None:
        # Match packaged processing: incompatible runtime type decorators must
        # not prevent Roformer from loading under supported Python versions.
        launcher = Path(__file__).resolve().parents[1] / "launcher.py"
        entrypoint = (str(launcher),) if launcher.is_file() else ("-m", "ytultimate")
        return sys.executable, ("-O", "-X", "utf8", "-u", *entrypoint, "--audio-separator")
    return executable, arguments


def _default_library_path() -> Path:
    legacy = Path.home() / "Downloads" / "YT_Ultimate"
    if legacy.exists():
        return legacy
    return Path.home() / "Music" / APP_NAME


@dataclass(frozen=True, slots=True)
class AppConfig:
    """Immutable process configuration.

    User preferences live in SQLite. These values describe the runtime itself and
    are injected by Electron in packaged builds.
    """

    host: str
    port: int
    auth_token: str
    resource_dir: Path
    data_dir: Path
    cache_dir: Path
    log_dir: Path
    library_dir: Path
    model_dir: Path
    plugin_dir: Path
    database_path: Path
    ffmpeg_path: str
    ffprobe_path: str
    separator_path: str
    developer_mode_override: bool | None
    allowed_origins: tuple[str, ...]
    # Leading arguments for separator_path, e.g. an interpreter running the
    # bundled entry point when the separator is not a standalone executable.
    separator_args: tuple[str, ...] = ()
    fpcalc_path: str = ""

    @classmethod
    def from_env(cls) -> "AppConfig":
        package_root = Path(__file__).resolve().parents[1]
        resource_dir = _env_path("RESOURCE_DIR", package_root)
        data_dir = _env_path(
            "DATA_DIR",
            Path(user_data_path(APP_NAME, APP_AUTHOR, ensure_exists=False)),
        )
        cache_dir = _env_path(
            "CACHE_DIR",
            Path(user_cache_path(APP_NAME, APP_AUTHOR, ensure_exists=False)),
        )
        log_dir = _env_path(
            "LOG_DIR",
            Path(user_log_path(APP_NAME, APP_AUTHOR, ensure_exists=False)),
        )
        library_dir = _env_path("LIBRARY_DIR", _default_library_path())
        model_dir = _env_path("MODEL_DIR", data_dir / "Models")
        plugin_dir = _env_path("PLUGIN_DIR", data_dir / "Plugins")
        resource_bin = resource_dir / "bin"
        separator_path, separator_args = _separator_command(resource_bin)

        raw_override = os.getenv(f"{ENV_PREFIX}DEVELOPER_MODE")
        developer_mode_override = (
            _env_bool("DEVELOPER_MODE") if raw_override is not None else None
        )
        raw_origins = os.getenv(
            f"{ENV_PREFIX}ALLOWED_ORIGINS",
            "http://127.0.0.1:5173,http://localhost:5173,null",
        )
        allowed_origins = tuple(
            value.strip() for value in raw_origins.split(",") if value.strip()
        )

        try:
            port = int(os.getenv(f"{ENV_PREFIX}PORT", str(DEFAULT_PORT)))
        except ValueError as exc:
            raise ValueError("YTU_PORT must be an integer") from exc
        if not 0 <= port <= 65535:
            raise ValueError("YTU_PORT must be between 0 and 65535")

        return cls(
            host=os.getenv(f"{ENV_PREFIX}HOST", DEFAULT_HOST).strip() or DEFAULT_HOST,
            port=port,
            auth_token=os.getenv(f"{ENV_PREFIX}AUTH_TOKEN", "").strip(),
            resource_dir=resource_dir,
            data_dir=data_dir,
            cache_dir=cache_dir,
            log_dir=log_dir,
            library_dir=library_dir,
            model_dir=model_dir,
            plugin_dir=plugin_dir,
            database_path=_env_path("DATABASE_PATH", data_dir / "yt-ultimate.sqlite3"),
            ffmpeg_path=_executable("ffmpeg", "FFMPEG_PATH", resource_bin),
            ffprobe_path=_executable("ffprobe", "FFPROBE_PATH", resource_bin),
            separator_path=separator_path,
            developer_mode_override=developer_mode_override,
            allowed_origins=allowed_origins,
            separator_args=separator_args,
            fpcalc_path=_executable("fpcalc", "FPCALC_PATH", resource_bin),
        )

    def ensure_directories(self) -> None:
        for directory in (
            self.data_dir,
            self.cache_dir,
            self.log_dir,
            self.library_dir,
            self.model_dir,
            self.plugin_dir,
            self.database_path.parent,
        ):
            directory.mkdir(parents=True, exist_ok=True)

    @property
    def thumbnail_dir(self) -> Path:
        return self.data_dir / "Artwork"

    @property
    def stems_dir(self) -> Path:
        return self.library_dir / "Separaciones"

    @property
    def protected_dir(self) -> Path:
        return self.library_dir / "Protegidos"

    @property
    def chipmunk_dir(self) -> Path:
        return self.library_dir / "Ardillas"

    @property
    def separator_model_path(self) -> Path:
        managed = self.model_dir / DEFAULT_SEPARATOR_MODEL
        bundled = self.resource_dir / "models" / DEFAULT_SEPARATOR_MODEL
        return managed if managed.exists() or not bundled.exists() else bundled

    @property
    def plugin_examples_dir(self) -> Path:
        source = self.resource_dir / "plugin-examples"
        bundled = self.resource_dir / "docs" / "plugin-examples"
        return source if source.is_dir() or not bundled.is_dir() else bundled

    def ensure_feature_directories(self) -> None:
        for directory in (
            self.thumbnail_dir,
            self.stems_dir,
            self.protected_dir,
            self.chipmunk_dir,
        ):
            directory.mkdir(parents=True, exist_ok=True)
