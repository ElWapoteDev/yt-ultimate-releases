from __future__ import annotations

import importlib.util
import os
import shutil
import time
from pathlib import Path
from typing import Any

from fastapi import APIRouter

from ... import __version__
from ...database import Database
from ..dependencies import ContainerDependency


router = APIRouter(prefix="/system", tags=["system"])


def _command_available(value: str) -> bool:
    if not value:
        return False
    path = Path(value)
    if path.is_absolute() or os.sep in value:
        return path.is_file() and os.access(path, os.X_OK)
    return shutil.which(value) is not None


def _engine_report(container: Any) -> dict[str, bool]:
    return {
        "ytdlp": importlib.util.find_spec("yt_dlp") is not None,
        "pytube": importlib.util.find_spec("pytubefix") is not None,
        "ffmpeg": _command_available(container.config.ffmpeg_path),
        "ffprobe": _command_available(container.config.ffprobe_path),
        "separator": _command_available(container.config.separator_path),
    }


@router.get("/health")
def health(container: ContainerDependency) -> dict[str, Any]:
    database = container.require("database", Database)
    paths = container.path_report()
    return {
        "status": "ok",
        "service": "YT Ultimate",
        "version": __version__,
        "instance_id": container.instance_id,
        "uptime_seconds": max(
            0.0,
            time.time() - float(container.started_at or time.time()),
        ),
        "database_schema": database.schema_version,
        "database_integrity": database.integrity_check(),
        "developer_mode": container.plugin_manager is not None,
        "engines": _engine_report(container),
        "youtube": container.youtube.diagnostics() if container.youtube else {},
        "paths": paths,
        "library_path": paths["library"],
    }


@router.get("/paths")
def paths(container: ContainerDependency) -> dict[str, Any]:
    report = container.path_report()
    return {
        **report,
        "paths": report,
        "writable": {
            "data": os.access(container.config.data_dir, os.W_OK),
            "cache": os.access(container.config.cache_dir, os.W_OK),
            "library": os.access(container.config.library_dir, os.W_OK),
            "plugins": os.access(container.config.plugin_dir, os.W_OK),
        },
    }


__all__ = ["router"]
