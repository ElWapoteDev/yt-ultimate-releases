"""Explicit dependency container and application lifecycle."""

from __future__ import annotations

import asyncio
import os
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, cast

from . import __version__
from .config import AppConfig
from .database import Database
from .events import EventBus
from .jobs import JobManager
from .plugins import PluginManager
from .services.downloads import DownloadService
from .services.library import LibraryService
from .services.media import MediaTools
from .services.playlists import PlaylistService
from .services.processing import ProcessingService
from .services.search import SearchService
from .services.settings import SettingsService
from .services.streaming import StreamingError, StreamingService
from .services.youtube import YoutubeAccessPolicy


class ContainerNotStartedError(RuntimeError):
    """Raised when a request reaches a container outside its lifespan."""


class _UnavailableStreamExtractor:
    def extract(self, _youtube_url: str) -> dict[str, Any]:
        raise StreamingError("No streaming engine is installed")


@dataclass(slots=True)
class Container:
    """Own all process-scoped services and close them in one place.

    The constructor is deliberately side-effect free. Directories, SQLite and
    optional plugin code are initialized only from :meth:`start`, which is
    called by FastAPI's lifespan.
    """

    config: AppConfig
    instance_id: str = field(
        default_factory=lambda: (
            os.getenv("YTU_INSTANCE_ID", "").strip() or str(uuid.uuid4())
        )
    )
    database: Database | None = field(default=None, init=False)
    event_bus: EventBus | None = field(default=None, init=False)
    settings: SettingsService | None = field(default=None, init=False)
    jobs: JobManager | None = field(default=None, init=False)
    media: MediaTools | None = field(default=None, init=False)
    library: LibraryService | None = field(default=None, init=False)
    playlists: PlaylistService | None = field(default=None, init=False)
    search: SearchService | None = field(default=None, init=False)
    downloads: DownloadService | None = field(default=None, init=False)
    processing: ProcessingService | None = field(default=None, init=False)
    streaming: StreamingService | None = field(default=None, init=False)
    youtube: YoutubeAccessPolicy | None = field(default=None, init=False)
    plugin_manager: PluginManager | None = field(default=None, init=False)
    startup_report: dict[str, Any] = field(default_factory=dict, init=False)
    started_at: float | None = field(default=None, init=False)
    _lifecycle_lock: asyncio.Lock = field(
        default_factory=asyncio.Lock,
        init=False,
        repr=False,
    )
    _event_loop: asyncio.AbstractEventLoop | None = field(
        default=None,
        init=False,
        repr=False,
    )
    _plugin_lock: asyncio.Lock = field(
        default_factory=asyncio.Lock,
        init=False,
        repr=False,
    )

    @property
    def started(self) -> bool:
        return self.started_at is not None

    async def start(self) -> "Container":
        async with self._lifecycle_lock:
            if self.started:
                return self

            self.config.ensure_directories()
            self.config.ensure_feature_directories()
            self._event_loop = asyncio.get_running_loop()
            try:
                self.database = Database(self.config)
                self.database.migrate()
                self.settings = SettingsService(
                    self.config,
                    self.database,
                )
                settings_service = self.settings
                self.youtube = YoutubeAccessPolicy(
                    self.config, settings=self.settings.get_all
                )
                self.event_bus = EventBus()
                self.media = MediaTools(self.config)
                self.library = LibraryService(
                    self.config,
                    self.database,
                )
                self.playlists = PlaylistService(self.database)
                self.search = SearchService()
                effective_settings = self.settings.get_all()
                self.startup_report = await asyncio.to_thread(
                    self._prepare_library,
                    effective_settings,
                )
                effective_settings = self.settings.get_all()
                max_workers = int(effective_settings["max_workers"])
                self.jobs = JobManager(
                    max_workers=max_workers,
                    max_active_jobs=max(64, max_workers * 8),
                    database=self.database,
                )
                self.downloads = DownloadService(
                    self.config,
                    self.jobs,
                    self.media,
                    settings=self.settings.get_all,
                    registrar=self.library,
                    events=self.publish_event,
                    concurrency_limit=lambda: int(
                        settings_service.get("max_concurrent_downloads")
                    ),
                    policy=self.youtube,
                )
                self.processing = ProcessingService(
                    self.config,
                    self.jobs,
                    self.media,
                    registrar=self.library,
                    events=self.publish_event,
                    concurrency_limit=lambda: int(
                        settings_service.get("max_concurrent_processing")
                    ),
                )
                try:
                    self.streaming = StreamingService(access_policy=self.youtube)
                except ImportError:
                    self.streaming = StreamingService(
                        extractor=_UnavailableStreamExtractor(),
                        access_policy=self.youtube,
                    )
                if bool(effective_settings["developer_mode"]):
                    await self.set_developer_mode(True)
            except BaseException:
                await self._close_initialized()
                raise

            self.started_at = time.time()
            await self.require("event_bus", EventBus).emit(
                "app.started",
                {
                    "version": __version__,
                    "instance_id": self.instance_id,
                    "started_at": self.started_at,
                },
                source="application",
            )
            return self

    def _prepare_library(self, settings: dict[str, Any]) -> dict[str, Any]:
        library = self.require("library", LibraryService)
        legacy_files = tuple(
            self.config.library_dir / name
            for name in ("metadata.json", "playlists.json", "settings.json")
        )
        if any(path.is_file() for path in legacy_files):
            return {
                "operation": "legacy_migration",
                **library.migrate_legacy_json(),
            }
        if bool(settings.get("scan_library_on_startup", True)):
            return {
                "operation": "reconcile",
                **library.reconcile(probe=False),
            }
        return {"operation": "skipped"}

    async def set_developer_mode(self, enabled: bool) -> PluginManager | None:
        """Start or tear down the trusted in-process plugin runtime."""

        async with self._plugin_lock:
            if enabled:
                if self.plugin_manager is None:
                    manager = PluginManager(
                        self.config.plugin_dir,
                        developer_mode=True,
                        event_bus=self.require("event_bus", EventBus),
                        data_dir=self.config.data_dir / "Plugin Data",
                    )
                    self.plugin_manager = manager
                    try:
                        await manager.start()
                    except BaseException:
                        self.plugin_manager = None
                        await manager.stop()
                        raise
                return self.plugin_manager

            manager = self.plugin_manager
            self.plugin_manager = None
            if manager is not None:
                await manager.stop()
            return None

    def publish_event(
        self,
        event_name: str,
        payload: dict[str, Any] | Any,
    ) -> None:
        """Bridge synchronous worker callbacks into the shared async bus."""

        bus = self.event_bus
        loop = self._event_loop
        if bus is None or loop is None or loop.is_closed():
            return
        coroutine = bus.emit(
            event_name,
            payload,
            source="core",
        )
        try:
            running = asyncio.get_running_loop()
        except RuntimeError:
            running = None
        if running is loop:
            loop.create_task(coroutine)
        else:
            asyncio.run_coroutine_threadsafe(coroutine, loop)

    def require(self, name: str, expected_type: type[Any]) -> Any:
        value = getattr(self, name, None)
        if value is None:
            raise ContainerNotStartedError(
                f"Application service {name!r} is not initialized"
            )
        if not isinstance(value, expected_type):
            raise TypeError(f"Unexpected service type for {name!r}")
        return cast(Any, value)

    async def shutdown(self) -> None:
        async with self._lifecycle_lock:
            if not any(
                (
                    self.database,
                    self.settings,
                    self.jobs,
                    self.plugin_manager,
                )
            ):
                self.started_at = None
                return
            await self._close_initialized()

    async def _close_initialized(self) -> None:
        manager = self.plugin_manager
        self.plugin_manager = None
        if manager is not None and self.event_bus is not None and self.started_at is not None:
            try:
                await self.event_bus.emit(
                    "app.stopping",
                    {
                        "version": __version__,
                        "instance_id": self.instance_id,
                    },
                    source="application",
                )
            except Exception:
                pass
        if manager is not None:
            try:
                await manager.stop()
            except Exception:
                pass

        jobs = self.jobs
        self.jobs = None
        if jobs is not None:
            try:
                await asyncio.to_thread(
                    jobs.shutdown,
                    wait=True,
                    cancel_running=True,
                )
            except Exception:
                pass

        streaming = self.streaming
        self.streaming = None
        if streaming is not None:
            try:
                streaming.close()
            except Exception:
                pass

        self.processing = None
        self.downloads = None
        self.youtube = None
        self.search = None
        self.playlists = None
        self.media = None
        self.library = None
        self.settings = None
        if self.event_bus is not None:
            self.event_bus.clear()
        self.event_bus = None
        self._event_loop = None

        database = self.database
        self.database = None
        if database is not None:
            try:
                database.checkpoint()
            finally:
                database.close()
        self.started_at = None

    def path_report(self) -> dict[str, str]:
        return {
            "resources": str(self.config.resource_dir),
            "data": str(self.config.data_dir),
            "cache": str(self.config.cache_dir),
            "logs": str(self.config.log_dir),
            "library": str(self.config.library_dir),
            "models": str(self.config.model_dir),
            "plugins": str(self.config.plugin_dir),
            "database": str(self.config.database_path),
            "artwork": str(self.config.thumbnail_dir),
            "stems": str(self.config.stems_dir),
            "protected": str(self.config.protected_dir),
        }

    def is_path_configured(self, value: str) -> bool:
        if not value:
            return False
        path = Path(value)
        return path.is_file() if path.is_absolute() else bool(value)


__all__ = ["Container", "ContainerNotStartedError"]
