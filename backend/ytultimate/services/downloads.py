from __future__ import annotations

import os
import shutil
import string
import time
import uuid
from hashlib import sha256
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping, NoReturn, Protocol
from urllib.parse import urlparse

from ytultimate.config import AppConfig, SUPPORTED_MEDIA_EXTENSIONS
from ytultimate.jobs import (
    ConcurrencyLimit,
    ConcurrencyLimiter,
    JobCancelled,
    JobContext,
    JobManager,
    JobSnapshot,
)
from ytultimate.services.media import (
    MediaReadyCallback,
    MediaRegistrar,
    MediaTools,
    atomic_publish,
    dispatch_event,
    managed_content_path,
    register_media,
    reserve_unique_path,
    safe_filename,
)
from ytultimate.services.search import (
    extract_video_id,
    format_duration,
    normalize_playlist_url,
    validate_youtube_url,
)
from ytultimate.services.youtube import YoutubeAccessPolicy, YoutubeFailure


class DownloadError(RuntimeError):
    """Base error for download orchestration."""

    def __init__(self, message: str, *, failure: YoutubeFailure | None = None) -> None:
        super().__init__(message)
        self.failure = failure


class DownloadEngineUnavailable(DownloadError):
    """Raised when no download adapter is installed."""


ProgressCallback = Callable[[Mapping[str, Any]], None]
EventCallback = Callable[[str, Mapping[str, Any]], None]


@dataclass(frozen=True, slots=True)
class DownloadArtifact:
    path: Path
    title: str
    channel: str = ""
    duration: float = 0.0
    thumbnail: str = ""
    source_url: str = ""
    engine: str = ""


@dataclass(frozen=True, slots=True)
class PlaylistEntry:
    id: str
    url: str
    title: str
    channel: str = ""
    duration: float = 0.0
    thumbnail: str = ""

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "url": self.url,
            "title": self.title,
            "channel": self.channel,
            "duration": format_duration(self.duration),
            "duration_seconds": self.duration,
            "thumbnail": self.thumbnail,
        }


@dataclass(frozen=True, slots=True)
class PlaylistInfo:
    title: str
    channel: str
    thumbnail: str
    entries: tuple[PlaylistEntry, ...]

    @property
    def count(self) -> int:
        return len(self.entries)

    def to_dict(self) -> dict[str, Any]:
        return {
            "title": self.title,
            "channel": self.channel,
            "thumbnail": self.thumbnail,
            "count": self.count,
            "entries": [entry.to_dict() for entry in self.entries],
        }


class DownloadAdapter(Protocol):
    name: str

    def download(
        self,
        url: str,
        working_directory: Path,
        *,
        media_type: str,
        quality: str,
        context: JobContext,
        progress: ProgressCallback,
    ) -> DownloadArtifact: ...


class PlaylistAdapter(Protocol):
    def playlist_info(self, url: str, *, limit: int = 500) -> PlaylistInfo: ...


class ArtworkFetcher(Protocol):
    def fetch(self, url: str, *, context: JobContext) -> Path | None: ...


class RequestsArtworkFetcher:
    """Bounded HTTPS artwork downloader for trusted YouTube image hosts."""

    _HOST_SUFFIXES = (
        "ggpht.com",
        "googleusercontent.com",
        "ytimg.com",
    )
    _CONTENT_TYPES = {
        "image/avif": ".avif",
        "image/jpeg": ".jpg",
        "image/png": ".png",
        "image/webp": ".webp",
    }

    def __init__(
        self,
        directory: Path,
        *,
        requester: Callable[..., Any] | None = None,
        max_bytes: int = 10 * 1024 * 1024,
        timeout: tuple[float, float] = (10, 20),
        user_agent: str = "YT Ultimate/4.0",
    ) -> None:
        if max_bytes < 1024:
            raise ValueError("max_bytes must be at least 1024")
        if requester is None:
            import requests

            requester = requests.get
        self.directory = Path(directory)
        self.requester = requester
        self.max_bytes = max_bytes
        self.timeout = timeout
        self.user_agent = user_agent

    def fetch(self, url: str, *, context: JobContext) -> Path | None:
        if not self._trusted_url(url):
            return None
        context.raise_if_cancelled()
        response = self.requester(
            url,
            stream=True,
            timeout=self.timeout,
            allow_redirects=True,
            headers={"User-Agent": self.user_agent, "Accept": "image/*"},
        )
        temporary: Path | None = None
        try:
            if not self._trusted_url(str(getattr(response, "url", url))):
                return None
            if int(getattr(response, "status_code", 0)) != 200:
                return None
            headers = {
                str(key).lower(): str(value)
                for key, value in getattr(response, "headers", {}).items()
            }
            content_type = headers.get("content-type", "").split(";", 1)[0].lower()
            extension = self._CONTENT_TYPES.get(content_type)
            if extension is None:
                return None
            try:
                declared_size = int(headers.get("content-length") or 0)
            except ValueError:
                declared_size = 0
            if declared_size > self.max_bytes:
                return None

            self.directory.mkdir(parents=True, exist_ok=True)
            digest = sha256(url.encode("utf-8")).hexdigest()[:32]
            destination = self.directory / f"{digest}{extension}"
            if destination.is_file() and destination.stat().st_size > 0:
                return destination
            temporary = self.directory / (
                f".{digest}.{uuid.uuid4().hex}.part{extension}"
            )
            written = 0
            with temporary.open("wb") as output:
                for chunk in response.iter_content(chunk_size=64 * 1024):
                    context.raise_if_cancelled()
                    if not chunk:
                        continue
                    written += len(chunk)
                    if written > self.max_bytes:
                        raise DownloadError("Artwork exceeds configured size limit")
                    output.write(chunk)
                output.flush()
                os.fsync(output.fileno())
            if written < 128:
                return None
            os.replace(temporary, destination)
            temporary = None
            return destination
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)
            close = getattr(response, "close", None)
            if callable(close):
                close()

    def _trusted_url(self, url: str) -> bool:
        parsed = urlparse(url)
        host = (parsed.hostname or "").lower().rstrip(".")
        if (
            parsed.scheme != "https"
            or not host
            or parsed.username
            or parsed.password
            or not any(
                host == suffix or host.endswith(f".{suffix}")
                for suffix in self._HOST_SUFFIXES
            )
        ):
            return False
        try:
            port = parsed.port
        except ValueError:
            return False
        if port not in {None, 443}:
            return False
        return True


@dataclass(frozen=True, slots=True)
class DownloadSettings:
    audio_bitrate: int = 192
    video_quality: str = "best"
    filename_template: str = "{title}"
    preserve_source_metadata: bool = True
    auto_thumbnail: bool = True

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any] | None) -> "DownloadSettings":
        value = value or {}
        raw_bitrate = value.get("audio_bitrate", value.get("audio_quality", 192))
        if str(raw_bitrate).strip().lower() == "best":
            bitrate = 320
        else:
            try:
                bitrate = int(raw_bitrate)
            except (TypeError, ValueError):
                bitrate = 192
        bitrate = max(64, min(320, bitrate))
        quality = _normalize_video_quality(
            str(value.get("video_quality") or "best")
        )
        filename_template = str(value.get("filename_template") or "{title}").strip()
        if not filename_template or len(filename_template) > 200:
            filename_template = "{title}"
        preserve_source_metadata = value.get("preserve_source_metadata", True)
        return cls(
            audio_bitrate=bitrate,
            video_quality=quality,
            filename_template=filename_template,
            preserve_source_metadata=bool(preserve_source_metadata),
            auto_thumbnail=bool(value.get("auto_thumbnail", True)),
        )


class YtDlpDownloadAdapter:
    name = "yt-dlp"

    def __init__(
        self,
        config: AppConfig,
        *,
        module: Any | None = None,
        policy: YoutubeAccessPolicy | None = None,
    ) -> None:
        if module is None:
            import yt_dlp as module  # type: ignore[no-redef]

        self.config = config
        self._module = module
        self.policy = policy or YoutubeAccessPolicy(config)

    def download(
        self,
        url: str,
        working_directory: Path,
        *,
        media_type: str,
        quality: str,
        context: JobContext,
        progress: ProgressCallback,
    ) -> DownloadArtifact:
        context.raise_if_cancelled()
        working_directory.mkdir(parents=True, exist_ok=True)

        def hook(payload: Mapping[str, Any]) -> None:
            context.raise_if_cancelled()
            status = payload.get("status")
            if status == "downloading":
                downloaded = _number(payload.get("downloaded_bytes"))
                total = _number(
                    payload.get("total_bytes")
                    or payload.get("total_bytes_estimate")
                )
                fraction = downloaded / total if total > 0 else 0.0
                progress(
                    {
                        "phase": "downloading",
                        "fraction": max(0.0, min(1.0, fraction)),
                        "downloaded_bytes": int(downloaded),
                        "total_bytes": int(total),
                        "speed": _number(payload.get("speed")),
                        "eta_seconds": _optional_float(payload.get("eta")),
                    }
                )
            elif status == "finished":
                progress({"phase": "processing", "fraction": 1.0})

        options: dict[str, Any] = {
            **self.policy.options(),
            "format": _yt_dlp_format(media_type, quality),
            "progress_hooks": [hook],
            "noplaylist": True,
            "continuedl": True,
            "overwrites": False,
            "skip_unavailable_fragments": False,
        }
        if media_type == "video":
            options["merge_output_format"] = "mp4"
        if self.config.ffmpeg_path:
            ffmpeg = Path(self.config.ffmpeg_path)
            options["ffmpeg_location"] = str(
                ffmpeg.parent if ffmpeg.parent != Path(".") else ffmpeg
            )
        # A rejected signed URL requires a new extraction. Each attempt owns
        # its files: a format selected by the next extraction may be different.
        # Transport retries/resume remain the downloader's responsibility.
        for attempt in range(2):
            context.raise_if_cancelled()
            attempt_directory = working_directory / f"attempt-{attempt + 1}"
            attempt_directory.mkdir(parents=True, exist_ok=True)
            options["outtmpl"] = str(attempt_directory / "source.%(ext)s")
            try:
                with self._module.YoutubeDL(options) as downloader:
                    info = downloader.extract_info(url, download=True)
                break
            except JobCancelled:
                raise
            except Exception as exc:
                context.raise_if_cancelled()
                failure = YoutubeFailure.from_exception(exc)
                shutil.rmtree(attempt_directory, ignore_errors=True)
                if attempt or failure.code not in {"forbidden", "expired"}:
                    raise failure from exc
                progress({"phase": "retrying", "attempt": 2, "fraction": 0.0})
        context.raise_if_cancelled()
        output = _find_download_output(attempt_directory)
        if output is None:
            raise DownloadError("yt-dlp completed without producing a media file")
        return DownloadArtifact(
            path=output,
            title=str((info or {}).get("title") or "download"),
            channel=str(
                (info or {}).get("channel")
                or (info or {}).get("uploader")
                or ""
            ),
            duration=_number((info or {}).get("duration")),
            thumbnail=str((info or {}).get("thumbnail") or ""),
            source_url=url,
            engine=self.name,
        )

    def playlist_info(self, url: str, *, limit: int = 500) -> PlaylistInfo:
        options = {
            **self.policy.options(),
            "extract_flat": "in_playlist",
            "skip_download": True,
            "playlistend": limit,
        }
        with self._module.YoutubeDL(options) as downloader:
            payload = downloader.extract_info(url, download=False) or {}
        entries: list[PlaylistEntry] = []
        for raw in payload.get("entries") or []:
            if not isinstance(raw, Mapping):
                continue
            identifier = str(raw.get("id") or "")
            raw_url = str(raw.get("url") or raw.get("webpage_url") or "")
            if len(identifier) != 11:
                identifier = extract_video_id(raw_url) or ""
            if not identifier:
                continue
            thumbnails = raw.get("thumbnails") or []
            thumbnail = str(raw.get("thumbnail") or "")
            if not thumbnail and thumbnails and isinstance(thumbnails[-1], Mapping):
                thumbnail = str(thumbnails[-1].get("url") or "")
            entries.append(
                PlaylistEntry(
                    id=identifier,
                    url=f"https://www.youtube.com/watch?v={identifier}",
                    title=str(raw.get("title") or "Untitled"),
                    channel=str(raw.get("channel") or raw.get("uploader") or ""),
                    duration=_number(raw.get("duration")),
                    thumbnail=thumbnail,
                )
            )
        thumbnails = payload.get("thumbnails") or []
        thumbnail = str(payload.get("thumbnail") or "")
        if not thumbnail and thumbnails and isinstance(thumbnails[-1], Mapping):
            thumbnail = str(thumbnails[-1].get("url") or "")
        return PlaylistInfo(
            title=str(payload.get("title") or "Playlist"),
            channel=str(payload.get("channel") or payload.get("uploader") or ""),
            thumbnail=thumbnail,
            entries=tuple(entries[:limit]),
        )


class PytubeDownloadAdapter:
    name = "pytubefix"

    def __init__(
        self,
        media: MediaTools,
        *,
        youtube_class: Any | None = None,
        policy: YoutubeAccessPolicy | None = None,
    ) -> None:
        if youtube_class is None:
            from pytubefix import YouTube as youtube_class  # type: ignore[no-redef]

        self.media = media
        self._youtube_class = youtube_class
        self.policy = policy or YoutubeAccessPolicy(media.config)

    def download(
        self,
        url: str,
        working_directory: Path,
        *,
        media_type: str,
        quality: str,
        context: JobContext,
        progress: ProgressCallback,
    ) -> DownloadArtifact:
        working_directory.mkdir(parents=True, exist_ok=True)

        def on_progress(stream: Any, _chunk: bytes, remaining: int) -> None:
            context.raise_if_cancelled()
            total = _number(getattr(stream, "filesize", 0)) or 1.0
            downloaded = max(0.0, total - _number(remaining))
            progress(
                {
                    "phase": "downloading",
                    "fraction": downloaded / total,
                    "downloaded_bytes": int(downloaded),
                    "total_bytes": int(total),
                }
            )

        options = self.policy.options()
        # This fallback cannot represent an authenticated yt-dlp session. Never
        # silently switch identities or install process-global proxy handlers.
        if any(options.get(key) for key in ("cookiefile", "cookiesfrombrowser", "proxy")):
            raise DownloadError("La sesión configurada requiere el motor yt-dlp.")
        video = self._youtube_class(url, on_progress_callback=on_progress)
        context.raise_if_cancelled()
        if media_type == "audio":
            streams = video.streams.filter(only_audio=True).order_by("abr").desc()
            stream = streams.first()
            if stream is None:
                raise DownloadError("pytubefix found no audio stream")
            path = Path(
                stream.download(
                    output_path=str(working_directory),
                    filename=f"source.{getattr(stream, 'subtype', 'm4a')}",
                    timeout=options["socket_timeout"],
                    max_retries=options["retries"],
                    interrupt_checker=lambda: context.cancelled,
                )
            )
        else:
            stream = self._select_progressive_stream(video, quality)
            if stream is None:
                raise DownloadError("pytubefix found no compatible MP4 stream")
            path = Path(
                stream.download(
                    output_path=str(working_directory),
                    filename="source.mp4",
                    timeout=options["socket_timeout"],
                    max_retries=options["retries"],
                    interrupt_checker=lambda: context.cancelled,
                )
            )
        context.raise_if_cancelled()
        return DownloadArtifact(
            path=path,
            title=str(getattr(video, "title", "") or "download"),
            channel=str(getattr(video, "author", "") or ""),
            duration=_number(getattr(video, "length", 0)),
            thumbnail=str(getattr(video, "thumbnail_url", "") or ""),
            source_url=url,
            engine=self.name,
        )

    @staticmethod
    def _select_progressive_stream(video: Any, quality: str) -> Any | None:
        maximum = None if quality == "best" else int(quality)
        streams = list(
            video.streams.filter(progressive=True, file_extension="mp4")
            .order_by("resolution")
            .desc()
        )
        for stream in streams:
            resolution = str(getattr(stream, "resolution", "")).rstrip("p")
            try:
                height = int(resolution)
            except ValueError:
                continue
            if maximum is None or height <= maximum:
                return stream
        return None


class DownloadService:
    def __init__(
        self,
        config: AppConfig,
        jobs: JobManager,
        media: MediaTools,
        *,
        adapters: Iterable[DownloadAdapter] | None = None,
        settings: Mapping[str, Any]
        | Callable[[], Mapping[str, Any]]
        | None = None,
        registrar: MediaRegistrar | MediaReadyCallback | None = None,
        events: EventCallback | None = None,
        playlist_limit: int = 500,
        concurrency_limit: ConcurrencyLimit | None = None,
        artwork_fetcher: ArtworkFetcher | None = None,
        policy: YoutubeAccessPolicy | None = None,
    ) -> None:
        if playlist_limit < 1:
            raise ValueError("playlist_limit must be positive")
        self.config = config
        self.jobs = jobs
        self.media = media
        self._settings = settings
        self.policy = policy or YoutubeAccessPolicy(config, settings=settings)
        self.registrar = registrar
        self.events = events
        self.playlist_limit = playlist_limit
        self._concurrency = ConcurrencyLimiter(concurrency_limit)
        self.artwork_fetcher = artwork_fetcher or RequestsArtworkFetcher(
            config.thumbnail_dir
        )
        self.adapters = tuple(adapters) if adapters is not None else self._defaults()

    def _defaults(self) -> tuple[DownloadAdapter, ...]:
        adapters: list[DownloadAdapter] = []
        # yt-dlp is intentionally preferred. Each dependency is optional.
        try:
            adapters.append(YtDlpDownloadAdapter(self.config, policy=self.policy))
        except ImportError:
            pass
        try:
            adapters.append(PytubeDownloadAdapter(self.media, policy=self.policy))
        except ImportError:
            pass
        return tuple(adapters)

    def start_download(
        self,
        url: str,
        media_type: str = "audio",
        *,
        quality: str | None = None,
        title: str | None = None,
        thumbnail: str | None = None,
        destination: Path | None = None,
        callback: EventCallback | None = None,
    ) -> JobSnapshot:
        canonical_url = validate_youtube_url(url, allow_playlist=False)
        normalized_type = _normalize_media_type(media_type)
        target = self._destination_directory(destination)
        metadata = {
            "url": canonical_url,
            "type": normalized_type,
            "title": title or "Preparing…",
            "thumbnail": thumbnail or "",
            "phase": "pending",
            "filename": "",
            "engine": "",
        }
        snapshot = self.jobs.submit(
            "download",
            self._download_job,
            canonical_url,
            normalized_type,
            quality,
            title,
            thumbnail,
            target,
            callback,
            metadata=metadata,
        )
        self._emit("download.queued", snapshot.to_dict(), callback)
        return snapshot

    def start_playlist(
        self,
        url: str,
        media_type: str = "audio",
        *,
        quality: str | None = None,
        callback: EventCallback | None = None,
    ) -> JobSnapshot:
        canonical_url = normalize_playlist_url(url)
        normalized_type = _normalize_media_type(media_type)
        snapshot = self.jobs.submit(
            "playlist_download",
            self._playlist_job,
            canonical_url,
            normalized_type,
            quality,
            callback,
            metadata={
                "url": canonical_url,
                "type": normalized_type,
                "title": "Loading playlist…",
                "is_playlist": True,
                "playlist_total": 0,
                "playlist_current": 0,
                "phase": "pending",
            },
        )
        self._emit("playlist.queued", snapshot.to_dict(), callback)
        return snapshot

    def playlist_info(self, url: str) -> dict[str, Any]:
        canonical_url = normalize_playlist_url(url)
        return self._playlist_info_object(canonical_url).to_dict()

    def get(self, job_id: str) -> JobSnapshot:
        return self.jobs.get(job_id)

    def list(self) -> list[JobSnapshot]:
        snapshots = [
            *self.jobs.list(kind="download"),
            *self.jobs.list(kind="playlist_download"),
        ]
        snapshots.sort(key=lambda item: item.created_at, reverse=True)
        return [
            snapshot
            for snapshot in snapshots
            if not snapshot.metadata.get("dismissed")
        ]

    def cancel(self, job_id: str) -> bool:
        return self.jobs.cancel(job_id)

    def dismiss(self, job_id: str) -> bool:
        snapshot = self.jobs.get(job_id)
        if snapshot.kind not in {"download", "playlist_download"}:
            return False
        if snapshot.done:
            return self.jobs.remove(job_id)
        self.jobs.update(job_id, dismissed=True)
        if self.jobs.cancel(job_id):
            return True
        latest = self.jobs.get(job_id)
        return self.jobs.remove(job_id) if latest.done else True

    def clear_finished(self) -> int:
        return self.jobs.clear(kind="download") + self.jobs.clear(
            kind="playlist_download"
        )

    def _download_job(
        self,
        context: JobContext,
        url: str,
        media_type: str,
        quality: str | None,
        title_hint: str | None,
        thumbnail_hint: str | None,
        destination: Path,
        callback: EventCallback | None,
    ) -> Mapping[str, Any]:
        with self._concurrency.slot(context):
            settings = self._current_settings()
            chosen_quality = (
                _normalize_video_quality(quality)
                if quality is not None
                else settings.video_quality
            )
            return self._download_one(
                context,
                url,
                media_type,
                chosen_quality,
                settings.audio_bitrate,
                settings.filename_template,
                settings.preserve_source_metadata,
                settings.auto_thumbnail,
                destination,
                title_hint=title_hint,
                thumbnail_hint=thumbnail_hint,
                callback=callback,
                progress_start=0.0,
                progress_span=100.0,
            )

    def _playlist_job(
        self,
        context: JobContext,
        url: str,
        media_type: str,
        quality: str | None,
        callback: EventCallback | None,
    ) -> Mapping[str, Any]:
        with self._concurrency.slot(context):
            return self._playlist_job_in_slot(
                context,
                url,
                media_type,
                quality,
                callback,
            )

    def _playlist_job_in_slot(
        self,
        context: JobContext,
        url: str,
        media_type: str,
        quality: str | None,
        callback: EventCallback | None,
    ) -> Mapping[str, Any]:
        self._update(
            context,
            callback,
            event="playlist.progress",
            progress=1,
            stage="Reading playlist",
            message="Loading playlist metadata",
            phase="starting",
        )
        info = self._playlist_info_object(url)
        if not info.entries:
            raise DownloadError("The playlist is empty")
        settings = self._current_settings()
        chosen_quality = (
            _normalize_video_quality(quality)
            if quality is not None
            else settings.video_quality
        )
        playlist_directory = self.config.library_dir / safe_filename(
            info.title,
            fallback="Playlist",
        )
        playlist_directory.mkdir(parents=True, exist_ok=True)
        total = len(info.entries)
        self._update(
            context,
            callback,
            event="playlist.progress",
            progress=3,
            stage="Downloading playlist",
            message=f"0 of {total}",
            title=info.title,
            thumbnail=info.thumbnail,
            channel=info.channel,
            playlist_total=total,
            playlist_current=0,
            phase="downloading",
        )
        outputs: list[Mapping[str, Any]] = []
        errors: list[dict[str, str]] = []
        per_item = 96.0 / total
        for index, entry in enumerate(info.entries):
            context.raise_if_cancelled()
            start = 3.0 + index * per_item
            try:
                output = self._download_one(
                    context,
                    entry.url,
                    media_type,
                    chosen_quality,
                    settings.audio_bitrate,
                    settings.filename_template,
                    settings.preserve_source_metadata,
                    settings.auto_thumbnail,
                    playlist_directory,
                    title_hint=entry.title,
                    thumbnail_hint=entry.thumbnail,
                    callback=callback,
                    progress_start=start,
                    progress_span=per_item,
                    playlist_name=info.title,
                )
                outputs.append(output)
            except JobCancelled:
                raise
            except Exception as exc:
                errors.append({"title": entry.title, "error": str(exc)})
                failure = getattr(exc, "failure", None)
                if failure is not None and failure.code in {
                    "bot_detected", "rate_limited", "configuration", "javascript_unavailable"
                }:
                    # A session-wide rejection will affect the rest of the
                    # playlist. Preserve completed items and stop the batch.
                    if not outputs:
                        _raise_download_failure(failure)
                    return {
                        "playlist": info.title,
                        "directory": str(playlist_directory),
                        "completed": len(outputs),
                        "failed": len(errors),
                        "skipped": total - index - 1,
                        "stopped": True,
                        "stop_reason": str(failure),
                        "outputs": outputs,
                        "errors": errors,
                    }
            self._update(
                context,
                callback,
                event="playlist.progress",
                progress=start + per_item,
                stage="Downloading playlist",
                message=f"{index + 1} of {total}",
                playlist_total=total,
                playlist_current=index + 1,
                downloaded=f"{len(outputs)}/{total}",
                phase="downloading",
            )
        if not outputs:
            details = " | ".join(error["error"] for error in errors[:3])
            raise DownloadError(f"No playlist items could be downloaded: {details}")
        return {
            "playlist": info.title,
            "directory": str(playlist_directory),
            "completed": len(outputs),
            "failed": len(errors),
            "outputs": outputs,
            "errors": errors,
        }

    def _download_one(
        self,
        context: JobContext,
        url: str,
        media_type: str,
        quality: str,
        bitrate: int,
        filename_template: str,
        preserve_source_metadata: bool,
        auto_thumbnail: bool,
        destination: Path,
        *,
        title_hint: str | None,
        thumbnail_hint: str | None,
        callback: EventCallback | None,
        progress_start: float,
        progress_span: float,
        playlist_name: str = "",
    ) -> Mapping[str, Any]:
        if not self.adapters:
            raise DownloadEngineUnavailable("No download engine is installed")
        staging_root = (
            self.config.cache_dir / "Downloads" / context.job_id / os.urandom(6).hex()
        )
        staging_root.mkdir(parents=True, exist_ok=False)
        failures: list[YoutubeFailure] = []
        artifact: DownloadArtifact | None = None
        try:
            for adapter_index, adapter in enumerate(self.adapters):
                context.raise_if_cancelled()
                adapter_dir = staging_root / f"{adapter_index}-{adapter.name}"
                adapter_dir.mkdir(parents=True, exist_ok=True)

                def progress(payload: Mapping[str, Any]) -> None:
                    fraction = max(0.0, min(1.0, _number(payload.get("fraction"))))
                    job_progress = progress_start + progress_span * fraction * 0.82
                    fields = dict(payload)
                    fields.pop("fraction", None)
                    phase = str(fields.pop("phase", "downloading"))
                    raw_speed = fields.get("speed")
                    if raw_speed:
                        fields["speed"] = _format_speed(_number(raw_speed))
                    raw_eta = fields.pop("eta_seconds", None)
                    if raw_eta is not None:
                        fields["eta"] = _format_eta(raw_eta)
                    downloaded_bytes = _number(fields.get("downloaded_bytes"))
                    if downloaded_bytes:
                        fields["downloaded"] = _format_bytes(downloaded_bytes)
                    self._update(
                        context,
                        callback,
                        event="download.progress",
                        progress=job_progress,
                        stage="Downloading",
                        message=title_hint or "Downloading media",
                        phase=phase,
                        engine=adapter.name,
                        **fields,
                    )

                try:
                    artifact = adapter.download(
                        url,
                        adapter_dir,
                        media_type=media_type,
                        quality=quality,
                        context=context,
                        progress=progress,
                    )
                    if not artifact.path.is_file():
                        raise DownloadError(
                            f"{adapter.name} returned a missing output file"
                        )
                    break
                except JobCancelled:
                    raise
                except Exception as exc:
                    context.raise_if_cancelled()
                    failure = YoutubeFailure.from_exception(exc)
                    failures.append(failure)
                    context.update(
                        error_code=failure.code,
                        retryable=failure.retryable,
                        engine=adapter.name,
                    )
                    shutil.rmtree(adapter_dir, ignore_errors=True)
                    if failure.code in {"unavailable", "rate_limited", "configuration"}:
                        break
            if artifact is None:
                # Prefer the concrete cause over an uninformative fallback
                # error, and never persist signed URLs or cookies in job logs.
                failure = next(
                    (item for item in reversed(failures) if item.code != "extraction"),
                    failures[-1],
                )
                context.update(error_code=failure.code, retryable=failure.retryable)
                _raise_download_failure(failure)

            title = artifact.title or title_hint or "download"
            extension = ".mp3" if media_type == "audio" else ".mp4"
            rendered_name = _render_filename(
                filename_template,
                title=title,
                channel=artifact.channel,
                video_id=extract_video_id(url) or "",
                media_type=media_type,
                quality=str(bitrate if media_type == "audio" else quality),
            )
            destination.mkdir(parents=True, exist_ok=True)
            reserved = reserve_unique_path(
                destination,
                f"{safe_filename(rendered_name)}{extension}",
            )
            publish_temp = reserved.with_name(
                f".{reserved.stem}.{context.job_id}.publishing{extension}"
            )
            try:
                conversion_progress = progress_start + progress_span * 0.88
                self._update(
                    context,
                    callback,
                    event="download.progress",
                    progress=conversion_progress,
                    stage="Converting" if media_type == "audio" else "Finalizing",
                    message=f"Preparing {reserved.name}",
                    phase="converting" if media_type == "audio" else "processing",
                    title=title,
                    thumbnail=artifact.thumbnail or thumbnail_hint or "",
                    channel=artifact.channel,
                    engine=artifact.engine,
                )
                if media_type == "audio":
                    if artifact.path.suffix.lower() == ".mp3":
                        self.media.copy_file(
                            artifact.path,
                            publish_temp,
                            context=context,
                        )
                    else:
                        self.media.convert_to_mp3(
                            artifact.path,
                            publish_temp,
                            bitrate_kbps=bitrate,
                            context=context,
                        )
                elif artifact.path.suffix.lower() == ".mp4":
                    self.media.copy_file(
                        artifact.path,
                        publish_temp,
                        context=context,
                    )
                else:
                    self.media.remux_to_mp4(
                        artifact.path,
                        publish_temp,
                        context=context,
                    )
                duration = artifact.duration or self.media.duration(
                    publish_temp,
                    context=context,
                )
                context.raise_if_cancelled()
                atomic_publish(publish_temp, reserved)
            except Exception:
                publish_temp.unlink(missing_ok=True)
                reserved.unlink(missing_ok=True)
                raise

            thumbnail_url = (
                artifact.thumbnail or thumbnail_hint or ""
                if preserve_source_metadata
                else ""
            )
            thumbnail_local = ""
            if auto_thumbnail and thumbnail_url:
                try:
                    fetched_artwork = self.artwork_fetcher.fetch(
                        thumbnail_url,
                        context=context,
                    )
                    thumbnail_local = (
                        str(fetched_artwork) if fetched_artwork is not None else ""
                    )
                except JobCancelled:
                    raise
                except Exception:
                    thumbnail_local = ""
            metadata = {
                "title": title,
                "channel": artifact.channel if preserve_source_metadata else "",
                "duration": duration,
                "thumbnail_url": thumbnail_url,
                "thumbnail_local": thumbnail_local,
                "source_url": url,
                "type": media_type,
                "quality": bitrate if media_type == "audio" else quality,
                "downloaded_at": time.time(),
                "playlist": playlist_name,
                "engine": artifact.engine,
            }
            registration: Any = None
            try:
                registration = register_media(
                    self.registrar,
                    reserved,
                    metadata,
                )
            except Exception as exc:
                # The completed media remains valid; surface indexing failure as
                # metadata so a later reconcile can recover it.
                metadata["registration_error"] = str(exc)
            result = {
                "filename": reserved.name,
                "local_path": str(reserved),
                "path": managed_content_path(
                    self.config.library_dir,
                    reserved,
                ),
                "size_bytes": reserved.stat().st_size,
                "track_id": (
                    str(registration.get("id") or "")
                    if isinstance(registration, Mapping)
                    else ""
                ),
                **metadata,
            }
            self._update(
                context,
                callback,
                event="download.media_ready",
                progress=progress_start + progress_span,
                stage="Completed",
                message=reserved.name,
                phase="completed",
                filename=reserved.name,
                local_path=str(reserved),
                title=title,
            )
            return result
        finally:
            shutil.rmtree(staging_root, ignore_errors=True)

    def _playlist_info_object(self, url: str) -> PlaylistInfo:
        failures: list[YoutubeFailure] = []
        for adapter in self.adapters:
            method = getattr(adapter, "playlist_info", None)
            if not callable(method):
                continue
            try:
                info = method(url, limit=self.playlist_limit)
                if isinstance(info, PlaylistInfo):
                    return info
                raise TypeError("playlist adapter returned an invalid result")
            except Exception as exc:
                failure = YoutubeFailure.from_exception(exc)
                failures.append(failure)
                if failure.code in {"unavailable", "rate_limited", "bot_detected", "configuration"}:
                    break
        if not failures:
            raise DownloadEngineUnavailable(
                "No configured adapter can inspect playlists"
            )
        _raise_download_failure(failures[-1])

    def _destination_directory(self, destination: Path | None) -> Path:
        root = self.config.library_dir.expanduser().resolve()
        if destination is None:
            return root
        target = Path(destination).expanduser()
        if not target.is_absolute():
            target = root / target
        target = target.resolve()
        try:
            target.relative_to(root)
        except ValueError as exc:
            raise ValueError("Download destination must be inside the library") from exc
        return target

    def _current_settings(self) -> DownloadSettings:
        if callable(self._settings):
            source = self._settings()
        else:
            getter = getattr(self._settings, "get_all", None)
            source = getter() if callable(getter) else self._settings
        return DownloadSettings.from_mapping(source)

    def _update(
        self,
        context: JobContext,
        callback: EventCallback | None,
        *,
        event: str,
        progress: float | None = None,
        stage: str | None = None,
        message: str | None = None,
        **metadata: Any,
    ) -> JobSnapshot:
        snapshot = context.update(
            progress=progress,
            stage=stage,
            message=message,
            metadata=metadata,
        )
        self._emit(event, snapshot.to_dict(), callback)
        return snapshot

    def _emit(
        self,
        event: str,
        payload: Mapping[str, Any],
        callback: EventCallback | None = None,
    ) -> None:
        for target in (self.events, callback):
            dispatch_event(target, event, payload)


def _raise_download_failure(failure: YoutubeFailure) -> NoReturn:
    raise DownloadError(str(failure), failure=failure) from failure


def _normalize_media_type(value: str) -> str:
    normalized = (value or "").strip().lower()
    aliases = {"mp3": "audio", "mp4": "video"}
    normalized = aliases.get(normalized, normalized)
    if normalized not in {"audio", "video"}:
        raise ValueError("media_type must be 'audio' or 'video'")
    return normalized


def _normalize_video_quality(value: str) -> str:
    normalized = (value or "best").strip().lower().rstrip("p")
    if normalized == "best":
        return normalized
    try:
        height = int(normalized)
    except ValueError as exc:
        raise ValueError("video quality must be 'best' or a vertical resolution") from exc
    if not 144 <= height <= 4320:
        raise ValueError("video resolution must be between 144p and 4320p")
    return str(height)


def _yt_dlp_format(media_type: str, quality: str) -> str:
    if media_type == "audio":
        return "bestaudio[ext=m4a]/bestaudio/best"
    if quality == "best":
        return (
            "bestvideo[ext=mp4]+bestaudio[ext=m4a]/"
            "bestvideo+bestaudio/best[ext=mp4]/best"
        )
    return (
        f"bestvideo[height<={quality}][ext=mp4]+bestaudio[ext=m4a]/"
        f"bestvideo[height<={quality}]+bestaudio/"
        f"best[height<={quality}][ext=mp4]/best[height<={quality}]"
    )


def _find_download_output(directory: Path) -> Path | None:
    candidates = [
        path
        for path in directory.iterdir()
        if path.is_file()
        and path.stat().st_size > 0
        and path.stem == "source"
        and path.suffix.lower() in SUPPORTED_MEDIA_EXTENSIONS
        and not path.name.endswith((".part", ".ytdl", ".temp"))
    ]
    candidates.sort(key=lambda path: path.stat().st_mtime_ns, reverse=True)
    return candidates[0] if candidates else None


def _number(value: Any) -> float:
    try:
        return max(0.0, float(value or 0))
    except (TypeError, ValueError):
        return 0.0


def _optional_float(value: Any) -> float | None:
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def _format_speed(bytes_per_second: float) -> str:
    if bytes_per_second >= 1024 * 1024:
        return f"{bytes_per_second / (1024 * 1024):.1f} MB/s"
    if bytes_per_second >= 1024:
        return f"{bytes_per_second / 1024:.0f} KB/s"
    return f"{bytes_per_second:.0f} B/s"


def _format_bytes(value: float) -> str:
    if value >= 1024 * 1024 * 1024:
        return f"{value / (1024 * 1024 * 1024):.1f} GB"
    if value >= 1024 * 1024:
        return f"{value / (1024 * 1024):.1f} MB"
    if value >= 1024:
        return f"{value / 1024:.0f} KB"
    return f"{value:.0f} B"


def _format_eta(value: Any) -> str:
    try:
        total = max(0, int(float(value)))
    except (TypeError, ValueError):
        return ""
    minutes, seconds = divmod(total, 60)
    if minutes >= 60:
        hours, minutes = divmod(minutes, 60)
        return f"{hours}:{minutes:02d}:{seconds:02d}"
    return f"{minutes}:{seconds:02d}"


def _render_filename(template: str, **values: str) -> str:
    """Render a constrained user template without attribute/index traversal."""

    formatter = string.Formatter()
    try:
        parts = list(formatter.parse(template))
    except ValueError as exc:
        raise ValueError("Invalid filename template") from exc
    for _literal, field_name, format_spec, conversion in parts:
        if field_name is None:
            continue
        if field_name not in values:
            raise ValueError(f"Unknown filename template field: {field_name}")
        if format_spec or conversion:
            raise ValueError("Filename template formatting operators are not supported")
    try:
        rendered = template.format_map(values)
    except (KeyError, ValueError) as exc:
        raise ValueError("Invalid filename template") from exc
    return rendered or values.get("title") or "download"
