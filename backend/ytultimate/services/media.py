from __future__ import annotations

import asyncio
import inspect
import json
import mimetypes
import os
import queue
import re
import shutil
import subprocess
import threading
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping, Protocol, Sequence
from urllib.parse import quote

from ytultimate.config import (
    AppConfig,
    HIDDEN_PROCESS_OPTIONS,
    SUPPORTED_AUDIO_EXTENSIONS,
    SUPPORTED_MEDIA_EXTENSIONS,
)
from ytultimate.jobs import JobCancelled, JobContext


_ANSI_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")


class MediaToolError(RuntimeError):
    """Base error for local media operations."""


class ToolUnavailableError(MediaToolError):
    """Raised when a configured executable is unavailable."""


class CommandExecutionError(MediaToolError):
    def __init__(
        self,
        message: str,
        *,
        command: Sequence[str],
        returncode: int | None = None,
        stderr: str = "",
    ) -> None:
        super().__init__(message)
        self.command = tuple(command)
        self.returncode = returncode
        self.stderr = stderr


class MediaRegistrar(Protocol):
    def register_media(
        self,
        path: Path,
        metadata: Mapping[str, Any],
    ) -> Any: ...

    def reconcile(self) -> Any: ...


MediaReadyCallback = Callable[[Path, Mapping[str, Any]], Any]
LineCallback = Callable[[str], None]


class EventBusSink(Protocol):
    def emit_nowait(self, event_name: str, payload: Mapping[str, Any]) -> Any: ...

    def emit_sync(self, event_name: str, payload: Mapping[str, Any]) -> Any: ...


EventSink = Callable[[str, Mapping[str, Any]], Any] | EventBusSink


@dataclass(frozen=True, slots=True)
class CommandResult:
    command: tuple[str, ...]
    returncode: int
    stdout: str
    stderr: str


@dataclass(frozen=True, slots=True)
class MediaInfo:
    path: Path
    mime_type: str
    duration: float
    size_bytes: int
    format_name: str
    audio_codec: str
    video_codec: str
    width: int | None
    height: int | None
    raw_probe: Mapping[str, Any]

    @property
    def has_audio(self) -> bool:
        return bool(self.audio_codec)

    @property
    def has_video(self) -> bool:
        return bool(self.video_codec)

    def to_dict(self) -> dict[str, Any]:
        return {
            "path": str(self.path),
            "mime_type": self.mime_type,
            "duration": self.duration,
            "size_bytes": self.size_bytes,
            "format_name": self.format_name,
            "audio_codec": self.audio_codec,
            "video_codec": self.video_codec,
            "width": self.width,
            "height": self.height,
        }


_MIME_OVERRIDES = {
    ".aac": "audio/aac",
    ".flac": "audio/flac",
    ".m4a": "audio/mp4",
    ".mkv": "video/x-matroska",
    ".mp3": "audio/mpeg",
    ".mp4": "video/mp4",
    ".ogg": "audio/ogg",
    ".opus": "audio/ogg",
    ".wav": "audio/wav",
    ".webm": "video/webm",
}
_INVALID_FILENAME = re.compile(r'[<>:"/\\|?*\x00-\x1f\x7f]')
_SPACE_RUN = re.compile(r"\s+")
# Windows maps these names to devices even when an extension follows them.
_WINDOWS_FILENAMES = os.name == "nt"
_WINDOWS_RESERVED_NAMES = frozenset(
    {"CON", "PRN", "AUX", "NUL"}
    | {f"{device}{number}" for device in ("COM", "LPT") for number in range(1, 10)}
)


def safe_filename(value: str | None, *, fallback: str = "download", limit: int = 180) -> str:
    cleaned = _INVALID_FILENAME.sub("", value or "")
    cleaned = _SPACE_RUN.sub(" ", cleaned).strip().strip(".")
    result = (cleaned[:limit].rstrip() or fallback) if limit > 0 else fallback
    if (
        _WINDOWS_FILENAMES
        and result.split(".", 1)[0].rstrip().upper() in _WINDOWS_RESERVED_NAMES
    ):
        result = f"_{result}"
    return result


def unique_path(directory: Path, filename: str) -> Path:
    """Return a currently unused path.

    Use :func:`reserve_unique_path` when multiple workers may publish at once.
    """

    directory = Path(directory)
    filename = _safe_leaf_filename(filename)
    candidate = directory / filename
    stem, suffix = candidate.stem, candidate.suffix
    index = 2
    while candidate.exists():
        candidate = directory / f"{stem} {index}{suffix}"
        index += 1
    return candidate


def reserve_unique_path(directory: Path, filename: str) -> Path:
    """Atomically reserve a unique destination with an empty placeholder."""

    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    filename = _safe_leaf_filename(filename)
    stem = Path(filename).stem
    suffix = Path(filename).suffix
    index = 1
    while True:
        name = filename if index == 1 else f"{stem} {index}{suffix}"
        candidate = directory / name
        try:
            descriptor = os.open(candidate, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
        except FileExistsError:
            index += 1
            continue
        os.close(descriptor)
        return candidate


def atomic_publish(source: Path, destination: Path) -> Path:
    source = Path(source)
    destination = Path(destination)
    if not source.is_file():
        raise FileNotFoundError(source)
    destination.parent.mkdir(parents=True, exist_ok=True)
    os.replace(source, destination)
    return destination


def managed_content_path(
    library_root: Path,
    path: Path,
    *,
    url_prefix: str = "/files",
) -> str:
    """Build the public URL for a file managed below the library root."""

    root = Path(library_root).expanduser().resolve()
    candidate = Path(path).expanduser().resolve()
    try:
        relative = candidate.relative_to(root)
    except ValueError as exc:
        raise ValueError("Media path is outside the managed library") from exc
    prefix = "/" + url_prefix.strip("/")
    return f"{prefix}/{quote(relative.as_posix(), safe='/')}"


def register_media(
    registrar: MediaRegistrar | MediaReadyCallback | None,
    path: Path,
    metadata: Mapping[str, Any],
    *,
    reconcile: bool = False,
) -> Any:
    """Call a library boundary without importing its concrete implementation."""

    if registrar is None:
        return None
    if callable(registrar) and not hasattr(registrar, "register_media"):
        result = registrar(path, metadata)
    else:
        method = getattr(registrar, "register_media", None)
        if method is None:
            method = getattr(registrar, "register", None)
        if method is None:
            method = getattr(registrar, "registrar", None)
        if not callable(method):
            raise TypeError("registrar must be callable or expose register_media()")
        result = method(path, metadata)
    if reconcile:
        reconcile_method = getattr(registrar, "reconcile", None)
        if callable(reconcile_method):
            reconcile_method()
    return result


def dispatch_event(
    sink: EventSink | None,
    event_name: str,
    payload: Mapping[str, Any],
) -> None:
    """Dispatch to a callback or EventBus without coupling services to it."""

    if sink is None:
        return
    try:
        try:
            loop = asyncio.get_running_loop()
        except RuntimeError:
            loop = None
        emit_nowait = getattr(sink, "emit_nowait", None)
        emit_sync = getattr(sink, "emit_sync", None)
        if loop is not None and callable(emit_nowait):
            emit_nowait(event_name, payload)
            return
        if loop is None and callable(emit_sync):
            emit_sync(event_name, payload)
            return
        if not callable(sink):
            emitter = getattr(sink, "emit", None)
            if not callable(emitter):
                raise TypeError("event sink is not callable and exposes no emit()")
            result = emitter(event_name, payload)
        else:
            result = sink(event_name, payload)
        if inspect.isawaitable(result):
            if loop is not None:
                loop.create_task(result)
            else:
                asyncio.run(result)
    except Exception:
        # Events are observability/extension points and never own job outcomes.
        return


class MediaTools:
    """Safe, injected gateway to FFmpeg and FFprobe."""

    def __init__(
        self,
        config: AppConfig,
        *,
        process_factory: Callable[..., Any] = subprocess.Popen,
        monotonic: Callable[[], float] = time.monotonic,
    ) -> None:
        self.config = config
        self._process_factory = process_factory
        self._monotonic = monotonic

    @property
    def ffmpeg_path(self) -> str:
        return self.config.ffmpeg_path

    @property
    def ffprobe_path(self) -> str:
        return self.config.ffprobe_path

    def ffmpeg_available(self) -> bool:
        return self._tool_available(self.ffmpeg_path)

    def ffprobe_available(self) -> bool:
        return self._tool_available(self.ffprobe_path)

    @staticmethod
    def _tool_available(executable: str) -> bool:
        if not executable:
            return False
        path = Path(executable)
        if path.is_absolute() or path.parent != Path("."):
            return path.is_file() and os.access(path, os.X_OK)
        return shutil.which(executable) is not None

    def run_ffmpeg(
        self,
        arguments: Iterable[str | os.PathLike[str]],
        *,
        timeout: float | None = 900,
        context: JobContext | None = None,
        line_callback: LineCallback | None = None,
        cwd: Path | None = None,
    ) -> CommandResult:
        return self.run_executable(
            self.ffmpeg_path,
            arguments,
            timeout=timeout,
            context=context,
            line_callback=line_callback,
            cwd=cwd,
        )

    def run_ffprobe(
        self,
        arguments: Iterable[str | os.PathLike[str]],
        *,
        timeout: float | None = 20,
        context: JobContext | None = None,
    ) -> CommandResult:
        return self.run_executable(
            self.ffprobe_path,
            arguments,
            timeout=timeout,
            context=context,
        )

    def run_executable(
        self,
        executable: str,
        arguments: Iterable[str | os.PathLike[str]],
        *,
        timeout: float | None = 900,
        context: JobContext | None = None,
        line_callback: LineCallback | None = None,
        cwd: Path | None = None,
        env: Mapping[str, str] | None = None,
    ) -> CommandResult:
        if not executable:
            raise ToolUnavailableError("Required media executable is not configured")
        command = [str(executable), *(str(argument) for argument in arguments)]
        if any("\x00" in part for part in command):
            raise ValueError("Command arguments cannot contain NUL bytes")
        if timeout is not None and timeout <= 0:
            raise ValueError("timeout must be positive")
        if context is not None:
            context.raise_if_cancelled()

        stderr_target: Any = subprocess.PIPE
        if line_callback is not None:
            stderr_target = subprocess.STDOUT
        try:
            process = self._process_factory(
                command,
                cwd=str(cwd) if cwd is not None else None,
                env=dict(env) if env is not None else None,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=stderr_target,
                text=True,
                encoding="utf-8",
                errors="replace",
                bufsize=1,
                shell=False,
                **HIDDEN_PROCESS_OPTIONS,
            )
        except FileNotFoundError as exc:
            raise ToolUnavailableError(f"Executable not found: {executable}") from exc
        except OSError as exc:
            raise ToolUnavailableError(f"Cannot start executable: {executable}") from exc

        if context is not None:
            context.register_process(process)
        try:
            if line_callback is None:
                stdout, stderr = self._communicate(
                    process,
                    timeout=timeout,
                    context=context,
                )
            else:
                stdout = self._read_lines(
                    process,
                    callback=line_callback,
                    timeout=timeout,
                    context=context,
                )
                stderr = ""
            returncode = int(process.wait())
        except BaseException:
            self._stop_process(process)
            raise
        finally:
            if context is not None:
                context.unregister_process(process)

        result = CommandResult(
            command=tuple(command),
            returncode=returncode,
            stdout=stdout or "",
            stderr=stderr or "",
        )
        if context is not None:
            context.raise_if_cancelled()
        if returncode != 0:
            detail = (result.stderr or result.stdout).strip()
            display_detail = _ANSI_ESCAPE.sub("", detail).strip()
            raise CommandExecutionError(
                f"{Path(executable).name} exited with status {returncode}: "
                f"{display_detail[-500:]}",
                command=command,
                returncode=returncode,
                stderr=detail,
            )
        return result

    def _communicate(
        self,
        process: Any,
        *,
        timeout: float | None,
        context: JobContext | None,
    ) -> tuple[str, str]:
        started = self._monotonic()
        while True:
            if context is not None and context.cancelled:
                self._stop_process(process)
                raise JobCancelled(f"Job {context.job_id} was cancelled")
            remaining = None
            if timeout is not None:
                remaining = timeout - (self._monotonic() - started)
                if remaining <= 0:
                    self._stop_process(process)
                    raise CommandExecutionError(
                        f"Media command timed out after {timeout:g} seconds",
                        command=getattr(process, "args", ()),
                    )
            try:
                return process.communicate(
                    timeout=min(0.25, remaining) if remaining is not None else 0.25
                )
            except subprocess.TimeoutExpired:
                continue

    def _read_lines(
        self,
        process: Any,
        *,
        callback: LineCallback,
        timeout: float | None,
        context: JobContext | None,
    ) -> str:
        output_queue: queue.Queue[str | None] = queue.Queue()

        def reader() -> None:
            stream = process.stdout
            try:
                if stream is not None:
                    for line in iter(stream.readline, ""):
                        output_queue.put(line)
            finally:
                if stream is not None:
                    stream.close()
                output_queue.put(None)

        threading.Thread(target=reader, name="ytu-media-output", daemon=True).start()
        started = self._monotonic()
        collected: list[str] = []
        reader_done = False
        while not reader_done or process.poll() is None:
            if context is not None and context.cancelled:
                self._stop_process(process)
                raise JobCancelled(f"Job {context.job_id} was cancelled")
            if timeout is not None and self._monotonic() - started > timeout:
                self._stop_process(process)
                raise CommandExecutionError(
                    f"Media command timed out after {timeout:g} seconds",
                    command=getattr(process, "args", ()),
                )
            try:
                line = output_queue.get(timeout=0.1)
            except queue.Empty:
                continue
            if line is None:
                reader_done = True
                continue
            collected.append(line)
            callback(line.rstrip("\r\n"))

        while True:
            try:
                line = output_queue.get_nowait()
            except queue.Empty:
                break
            if line:
                collected.append(line)
                callback(line.rstrip("\r\n"))
        return "".join(collected)

    @staticmethod
    def _stop_process(process: Any) -> None:
        try:
            if process.poll() is not None:
                return
        except Exception:
            pass
        try:
            process.terminate()
            process.wait(timeout=0.75)
            return
        except Exception:
            pass
        try:
            process.kill()
        except Exception:
            pass

    def probe(
        self,
        path: Path,
        *,
        context: JobContext | None = None,
    ) -> Mapping[str, Any]:
        source = self.require_media_file(path)
        result = self.run_ffprobe(
            [
                "-v",
                "error",
                "-show_format",
                "-show_streams",
                "-of",
                "json",
                source,
            ],
            context=context,
        )
        try:
            value = json.loads(result.stdout)
        except (TypeError, json.JSONDecodeError) as exc:
            raise MediaToolError("FFprobe returned invalid JSON") from exc
        if not isinstance(value, dict):
            raise MediaToolError("FFprobe returned an unexpected payload")
        return value

    def duration(
        self,
        path: Path,
        *,
        context: JobContext | None = None,
        default: float = 0.0,
    ) -> float:
        try:
            payload = self.probe(path, context=context)
            raw = payload.get("format", {}).get("duration")
            if raw is None:
                for stream in payload.get("streams", []):
                    if stream.get("duration") is not None:
                        raw = stream["duration"]
                        break
            return max(0.0, float(raw))
        except (MediaToolError, OSError, TypeError, ValueError):
            return default

    def mime_type(self, path: Path) -> str:
        suffix = Path(path).suffix.lower()
        if suffix in _MIME_OVERRIDES:
            return _MIME_OVERRIDES[suffix]
        guessed, _ = mimetypes.guess_type(str(path))
        return guessed or "application/octet-stream"

    def inspect(
        self,
        path: Path,
        *,
        context: JobContext | None = None,
    ) -> MediaInfo:
        source = self.require_media_file(path)
        payload = self.probe(source, context=context)
        format_data = payload.get("format") or {}
        streams = payload.get("streams") or []
        audio = next(
            (stream for stream in streams if stream.get("codec_type") == "audio"),
            {},
        )
        video = next(
            (stream for stream in streams if stream.get("codec_type") == "video"),
            {},
        )
        try:
            duration = max(0.0, float(format_data.get("duration") or 0))
        except (TypeError, ValueError):
            duration = 0.0
        return MediaInfo(
            path=source,
            mime_type=self.mime_type(source),
            duration=duration,
            size_bytes=source.stat().st_size,
            format_name=str(format_data.get("format_name") or ""),
            audio_codec=str(audio.get("codec_name") or ""),
            video_codec=str(video.get("codec_name") or ""),
            width=_positive_int(video.get("width")),
            height=_positive_int(video.get("height")),
            raw_probe=payload,
        )

    @staticmethod
    def copy_file(
        source: Path,
        destination: Path,
        *,
        context: JobContext | None = None,
        chunk_size: int = 1024 * 1024,
    ) -> Path:
        """Copy a media file while remaining cooperatively cancellable."""

        if chunk_size < 64 * 1024:
            raise ValueError("chunk_size must be at least 64 KiB")
        source = Path(source)
        destination = Path(destination)
        if not source.is_file():
            raise FileNotFoundError(source)
        destination.parent.mkdir(parents=True, exist_ok=True)
        try:
            with source.open("rb") as input_file, destination.open("wb") as output_file:
                while True:
                    if context is not None:
                        context.raise_if_cancelled()
                    chunk = input_file.read(chunk_size)
                    if not chunk:
                        break
                    output_file.write(chunk)
                output_file.flush()
                os.fsync(output_file.fileno())
            shutil.copystat(source, destination, follow_symlinks=False)
        except Exception:
            destination.unlink(missing_ok=True)
            raise
        return destination

    @staticmethod
    def require_media_file(path: Path) -> Path:
        source = Path(path).expanduser().resolve()
        if not source.is_file():
            raise FileNotFoundError(source)
        if source.suffix.lower() not in SUPPORTED_MEDIA_EXTENSIONS:
            raise ValueError(f"Unsupported media extension: {source.suffix}")
        return source

    @staticmethod
    def resolve_inside(root: Path, path: Path) -> Path:
        base = Path(root).expanduser().resolve()
        candidate = Path(path).expanduser()
        if not candidate.is_absolute():
            candidate = base / candidate
        candidate = candidate.resolve()
        try:
            candidate.relative_to(base)
        except ValueError as exc:
            raise ValueError("Path escapes the configured media root") from exc
        return candidate

    def convert_to_mp3(
        self,
        source: Path,
        destination: Path,
        *,
        bitrate_kbps: int = 192,
        context: JobContext | None = None,
    ) -> Path:
        if not 64 <= int(bitrate_kbps) <= 320:
            raise ValueError("MP3 bitrate must be between 64 and 320 kbps")
        return self._atomic_ffmpeg_output(
            source,
            destination,
            [
                "-vn",
                "-c:a",
                "libmp3lame",
                "-b:a",
                f"{int(bitrate_kbps)}k",
                "-ar",
                "44100",
                "-ac",
                "2",
            ],
            context=context,
        )

    def transcode_audio(
        self,
        source: Path,
        destination: Path,
        *,
        context: JobContext | None = None,
        filters: str | None = None,
        metadata: Mapping[str, str] | None = None,
        progress_callback: LineCallback | None = None,
    ) -> Path:
        suffix = Path(destination).suffix.lower()
        if suffix not in SUPPORTED_AUDIO_EXTENSIONS:
            raise ValueError(f"Unsupported audio output extension: {suffix}")
        arguments: list[str] = ["-vn"]
        if filters:
            arguments.extend(["-af", filters])
        for key, value in (metadata or {}).items():
            arguments.extend(["-metadata", f"{key}={value}"])
        arguments.extend(self.audio_codec_arguments(suffix))
        if progress_callback is not None:
            arguments.extend(["-progress", "pipe:1", "-nostats"])
        return self._atomic_ffmpeg_output(
            source,
            destination,
            arguments,
            context=context,
            line_callback=progress_callback,
        )

    def merge_av(
        self,
        video: Path,
        audio: Path,
        destination: Path,
        *,
        context: JobContext | None = None,
    ) -> Path:
        video_source = self.require_media_file(video)
        audio_source = self.require_media_file(audio)
        destination = Path(destination)
        temporary = self._temporary_output(destination)
        temporary.parent.mkdir(parents=True, exist_ok=True)
        try:
            base = [
                "-y",
                "-hide_banner",
                "-nostdin",
                "-i",
                video_source,
                "-i",
                audio_source,
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
            ]
            try:
                self.run_ffmpeg(
                    [
                        *base,
                        "-c:v",
                        "copy",
                        "-c:a",
                        "aac",
                        "-b:a",
                        "192k",
                        "-movflags",
                        "+faststart",
                        temporary,
                    ],
                    context=context,
                )
            except CommandExecutionError:
                temporary.unlink(missing_ok=True)
                self.run_ffmpeg(
                    [
                        *base,
                        "-c:v",
                        "libx264",
                        "-preset",
                        "medium",
                        "-crf",
                        "23",
                        "-c:a",
                        "aac",
                        "-b:a",
                        "192k",
                        "-movflags",
                        "+faststart",
                        temporary,
                    ],
                    context=context,
                )
            os.replace(temporary, destination)
        except Exception:
            temporary.unlink(missing_ok=True)
            raise
        return destination

    def remux_to_mp4(
        self,
        source: Path,
        destination: Path,
        *,
        context: JobContext | None = None,
    ) -> Path:
        try:
            return self._atomic_ffmpeg_output(
                source,
                destination,
                [
                    "-map",
                    "0:v:0",
                    "-map",
                    "0:a:0?",
                    "-c:v",
                    "copy",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "192k",
                    "-movflags",
                    "+faststart",
                ],
                context=context,
            )
        except CommandExecutionError:
            return self._atomic_ffmpeg_output(
                source,
                destination,
                [
                    "-map",
                    "0:v:0",
                    "-map",
                    "0:a:0?",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "medium",
                    "-crf",
                    "23",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "192k",
                    "-movflags",
                    "+faststart",
                ],
                context=context,
            )

    def _atomic_ffmpeg_output(
        self,
        source: Path,
        destination: Path,
        output_arguments: Sequence[str],
        *,
        context: JobContext | None,
        line_callback: LineCallback | None = None,
    ) -> Path:
        source = self.require_media_file(source)
        destination = Path(destination)
        destination.parent.mkdir(parents=True, exist_ok=True)
        temporary = self._temporary_output(destination)
        try:
            self.run_ffmpeg(
                [
                    "-y",
                    "-hide_banner",
                    "-nostdin",
                    "-loglevel",
                    "error",
                    "-i",
                    source,
                    *output_arguments,
                    temporary,
                ],
                context=context,
                line_callback=line_callback,
            )
            if not temporary.is_file():
                raise MediaToolError("FFmpeg completed without creating output")
            os.replace(temporary, destination)
        except Exception:
            temporary.unlink(missing_ok=True)
            raise
        return destination

    @staticmethod
    def _temporary_output(destination: Path) -> Path:
        destination = Path(destination)
        return destination.with_name(
            f".{destination.stem}.{uuid.uuid4().hex}.part{destination.suffix}"
        )

    @staticmethod
    def audio_codec_arguments(extension: str) -> list[str]:
        extension = extension.lower()
        if extension == ".flac":
            return ["-c:a", "flac", "-compression_level", "8"]
        if extension == ".wav":
            return ["-c:a", "pcm_s16le"]
        if extension in {".m4a", ".aac"}:
            return ["-c:a", "aac", "-b:a", "192k"]
        if extension == ".ogg":
            return ["-c:a", "libvorbis", "-q:a", "5"]
        if extension == ".opus":
            return ["-c:a", "libopus", "-b:a", "160k"]
        return ["-c:a", "libmp3lame", "-b:a", "192k"]


def _positive_int(value: Any) -> int | None:
    try:
        converted = int(value)
    except (TypeError, ValueError):
        return None
    return converted if converted > 0 else None


def _safe_leaf_filename(filename: str) -> str:
    raw = str(filename)
    suffix = Path(raw).suffix
    if len(suffix) > 16 or _INVALID_FILENAME.search(suffix):
        suffix = ""
    stem = raw[: -len(suffix)] if suffix else raw
    return f"{safe_filename(stem)}{suffix.lower()}"
