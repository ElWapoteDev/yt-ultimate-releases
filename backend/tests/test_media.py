from __future__ import annotations

import asyncio
import dataclasses
import hashlib
import os
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any, Mapping
from unittest import mock

from ytultimate.config import AppConfig, DEFAULT_SEPARATOR_MODEL
from ytultimate.jobs import JobManager, JobState
from ytultimate.services import media as media_module
from ytultimate.services import processing as processing_module
from ytultimate.services.downloads import (
    DownloadArtifact,
    DownloadService,
    PlaylistEntry,
    PlaylistInfo,
)
from ytultimate.services.media import (
    CommandExecutionError,
    CommandResult,
    MediaTools,
    reserve_unique_path,
    safe_filename,
)
from ytultimate.services.processing import ProcessingService
from ytultimate.services.search import (
    InvalidYouTubeURLError,
    SearchService,
    validate_youtube_url,
)
from ytultimate.services.streaming import StreamingService


def _config(root: Path) -> AppConfig:
    return AppConfig(
        host="127.0.0.1",
        port=8000,
        auth_token="",
        resource_dir=root / "resources",
        data_dir=root / "data",
        cache_dir=root / "cache",
        log_dir=root / "logs",
        library_dir=root / "library",
        model_dir=root / "models",
        plugin_dir=root / "plugins",
        database_path=root / "data" / "app.sqlite3",
        ffmpeg_path="/bundle/bin/ffmpeg",
        ffprobe_path="/bundle/bin/ffprobe",
        separator_path="/bundle/bin/audio-separator",
        developer_mode_override=None,
        allowed_origins=("null",),
    )


class _ImmediateProcess:
    def __init__(
        self,
        command,
        *,
        stdout: str = "",
        stderr: str = "",
        returncode: int = 0,
    ) -> None:
        self.args = command
        self._stdout = stdout
        self._stderr = stderr
        self.returncode = returncode

    def communicate(self, timeout=None):
        return self._stdout, self._stderr

    def wait(self, timeout=None):
        return self.returncode

    def poll(self):
        return self.returncode

    def terminate(self):
        self.returncode = -15

    def kill(self):
        self.returncode = -9


class MediaToolsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.config = _config(self.root)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_safe_filename_and_atomic_unique_reservation(self) -> None:
        directory = self.root / "library"
        first = reserve_unique_path(directory, 'bad:/\\" song.mp3')
        second = reserve_unique_path(directory, first.name)
        self.assertEqual(safe_filename('bad:/\\" song'), "bad song")
        self.assertNotEqual(first, second)
        self.assertTrue(first.exists())
        self.assertTrue(second.exists())

    def test_windows_device_names_are_never_used_as_filenames(self) -> None:
        self.assertEqual(safe_filename("con"), "con")
        with mock.patch.object(media_module, "_WINDOWS_FILENAMES", True):
            self.assertEqual(safe_filename("con"), "_con")
            self.assertEqual(safe_filename("Aux. Remix"), "_Aux. Remix")
            self.assertEqual(safe_filename("LPT1"), "_LPT1")
            self.assertEqual(safe_filename("Con Calma"), "Con Calma")
            self.assertEqual(safe_filename("Console"), "Console")

    def test_separator_arguments_are_read_from_the_environment(self) -> None:
        with mock.patch.dict(
            os.environ,
            {"YTU_SEPARATOR_PATH": sys.executable,
             "YTU_SEPARATOR_ARGS": '["-I", "launcher.py", "--audio-separator"]'},
        ):
            config = AppConfig.from_env()
        self.assertEqual(
            config.separator_args,
            ("-I", "launcher.py", "--audio-separator"),
        )
        with mock.patch.dict(os.environ, {
            "YTU_SEPARATOR_PATH": sys.executable, "YTU_SEPARATOR_ARGS": "",
        }):
            self.assertEqual(AppConfig.from_env().separator_args, ())
        for invalid in ('"launcher.py"', "[1]", "not json"):
            with (
                mock.patch.dict(os.environ, {"YTU_SEPARATOR_ARGS": invalid}),
                self.assertRaises(ValueError),
            ):
                AppConfig.from_env()

    def test_default_separator_matches_optimized_packaged_runtime(self) -> None:
        with (
            mock.patch.dict(os.environ, {"YTU_SEPARATOR_PATH": "", "YTU_SEPARATOR_ARGS": ""}),
            mock.patch("ytultimate.config.importlib.util.find_spec", return_value=object()),
        ):
            config = AppConfig.from_env()
        self.assertEqual(config.separator_path, sys.executable)
        self.assertEqual(config.separator_args[:4], ("-O", "-X", "utf8", "-u"))
        self.assertEqual(config.separator_args[-1], "--audio-separator")
        self.assertTrue(Path(config.separator_args[-2]).is_absolute())
        self.assertEqual(Path(config.separator_args[-2]).name, "launcher.py")

    def test_configured_interpreter_preserves_venv_symlink_and_explicit_args(self) -> None:
        real = self.root / "python-real"
        real.touch()
        interpreter = self.root / "venv-python"
        interpreter.symlink_to(real)
        with mock.patch.dict(os.environ, {
            "YTU_SEPARATOR_PATH": str(interpreter), "YTU_SEPARATOR_ARGS": '["-O", "custom.py"]',
        }):
            config = AppConfig.from_env()
        self.assertEqual(config.separator_path, str(interpreter))
        self.assertNotEqual(config.separator_path, str(interpreter.resolve()))
        self.assertEqual(config.separator_args, ("-O", "custom.py"))

    def test_frozen_separator_keeps_standalone_command(self) -> None:
        with (
            mock.patch.dict(os.environ, {"YTU_SEPARATOR_PATH": "", "YTU_SEPARATOR_ARGS": ""}),
            mock.patch.object(sys, "frozen", True, create=True),
            mock.patch("ytultimate.config._executable", return_value="bundled/audio-separator"),
        ):
            config = AppConfig.from_env()
        self.assertEqual(config.separator_path, "bundled/audio-separator")
        self.assertEqual(config.separator_args, ())

    def test_command_errors_remove_terminal_colors_from_display(self) -> None:
        detail = "\x1b[1;35mRuntimeError\x1b[0m: Failed to instantiate Roformer model"
        media = MediaTools(self.config, process_factory=lambda command, **kwargs: _ImmediateProcess(
            command, stderr=detail, returncode=1,
        ))
        with self.assertRaises(CommandExecutionError) as raised:
            media.run_ffmpeg(["-i", "bad"])
        self.assertIn("RuntimeError: Failed to instantiate Roformer model", str(raised.exception))
        self.assertNotIn("\x1b", str(raised.exception))
        self.assertEqual(raised.exception.stderr, detail)

    def test_execution_uses_configured_binary_without_shell(self) -> None:
        captured: dict[str, Any] = {}

        def factory(command, **kwargs):
            captured["command"] = command
            captured["kwargs"] = kwargs
            return _ImmediateProcess(command, stdout="ffmpeg version test")

        media = MediaTools(self.config, process_factory=factory)
        result = media.run_ffmpeg(["-version"], timeout=1)

        self.assertEqual(captured["command"][0], self.config.ffmpeg_path)
        self.assertIs(captured["kwargs"]["shell"], False)
        self.assertEqual(result.returncode, 0)
        self.assertIn("ffmpeg version", result.stdout)

    def test_nonzero_command_has_structured_failure(self) -> None:
        def factory(command, **_kwargs):
            return _ImmediateProcess(
                command,
                stderr="invalid input",
                returncode=2,
            )

        media = MediaTools(self.config, process_factory=factory)
        with self.assertRaises(CommandExecutionError) as raised:
            media.run_ffmpeg(["-i", "bad"], timeout=1)
        self.assertEqual(raised.exception.returncode, 2)
        self.assertIn("invalid input", str(raised.exception))

    def test_probe_information_and_mime(self) -> None:
        source = self.root / "track.m4a"
        source.write_bytes(b"media")

        class StubMedia(MediaTools):
            def probe(self, path, *, context=None):
                return {
                    "format": {"duration": "61.25", "format_name": "mov,mp4"},
                    "streams": [
                        {"codec_type": "audio", "codec_name": "aac"},
                    ],
                }

        media = StubMedia(self.config)
        info = media.inspect(source)
        self.assertEqual(info.mime_type, "audio/mp4")
        self.assertAlmostEqual(info.duration, 61.25)
        self.assertEqual(info.audio_codec, "aac")
        self.assertFalse(info.has_video)

    def test_conversion_publishes_only_completed_output(self) -> None:
        source = self.root / "source.wav"
        destination = self.root / "out" / "song.mp3"
        source.write_bytes(b"source")

        class WritingMedia(MediaTools):
            def run_ffmpeg(self, arguments, **_kwargs):
                command = tuple(str(value) for value in arguments)
                Path(command[-1]).write_bytes(b"converted")
                return CommandResult(command, 0, "", "")

        media = WritingMedia(self.config)
        media.convert_to_mp3(source, destination, bitrate_kbps=256)
        self.assertEqual(destination.read_bytes(), b"converted")
        self.assertEqual(
            list(destination.parent.glob("*.part.mp3")),
            [],
        )


class SearchServiceTests(unittest.TestCase):
    def test_url_validation_canonicalizes_supported_forms(self) -> None:
        self.assertEqual(
            validate_youtube_url("https://youtu.be/dQw4w9WgXcQ?t=2"),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        )
        with self.assertRaises(InvalidYouTubeURLError):
            validate_youtube_url("https://example.com/watch?v=dQw4w9WgXcQ")

    def test_adapter_fallback_and_result_normalization(self) -> None:
        class Broken:
            name = "broken"

            def search(self, query, limit):
                raise RuntimeError("offline")

        class Working:
            name = "fake"

            def search(self, query, limit):
                return [
                    {
                        "id": "dQw4w9WgXcQ",
                        "title": query,
                        "duration": 125,
                        "view_count": 1500,
                    }
                ]

        service = SearchService([Broken(), Working()])
        results = service.search("A song", 5)
        self.assertEqual(results[0]["duration"], "2:05")
        self.assertEqual(results[0]["views"], "2K views")


class _FakeDownloadAdapter:
    name = "fake"

    def download(
        self,
        url,
        working_directory,
        *,
        media_type,
        quality,
        context,
        progress,
    ):
        progress({"phase": "downloading", "fraction": 0.5})
        output = working_directory / "source.mp3"
        output.write_bytes(b"audio bytes")
        return DownloadArtifact(
            path=output,
            title="Same Song",
            duration=10,
            source_url=url,
            engine=self.name,
        )


class _FakePlaylistAdapter(_FakeDownloadAdapter):
    def playlist_info(self, url, *, limit=500):
        del url, limit
        return PlaylistInfo(
            title="Headless Mix",
            channel="Tests",
            thumbnail="",
            entries=(
                PlaylistEntry(
                    id="dQw4w9WgXcQ",
                    url="https://youtu.be/dQw4w9WgXcQ",
                    title="First",
                ),
                PlaylistEntry(
                    id="9bZkp7q19f0",
                    url="https://youtu.be/9bZkp7q19f0",
                    title="Second",
                ),
            ),
        )


class DownloadServiceTests(unittest.TestCase):
    def test_download_jobs_use_unique_atomic_names_without_network(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            config = _config(Path(temporary))
            config.ensure_directories()
            registered: list[Path] = []
            with JobManager(max_workers=2, max_active_jobs=4) as jobs:
                service = DownloadService(
                    config,
                    jobs,
                    MediaTools(config),
                    adapters=[_FakeDownloadAdapter()],
                    registrar=lambda path, metadata: registered.append(path),
                )
                first = service.start_download(
                    "https://youtu.be/dQw4w9WgXcQ",
                    "audio",
                )
                second = service.start_download(
                    "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
                    "mp3",
                )
                first_done = jobs.wait(first.id, timeout=3)
                second_done = jobs.wait(second.id, timeout=3)

            self.assertEqual(first_done.status, JobState.COMPLETED)
            self.assertEqual(second_done.status, JobState.COMPLETED)
            self.assertEqual(
                {path.name for path in registered},
                {"Same Song.mp3", "Same Song 2.mp3"},
            )
            self.assertTrue(all(path.read_bytes() == b"audio bytes" for path in registered))

    def test_playlist_keeps_downloading_phase_between_completed_items(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            config = _config(Path(temporary))
            config.ensure_directories()
            events: list[tuple[str, Mapping[str, Any]]] = []
            with JobManager(max_workers=1, max_active_jobs=2) as jobs:
                service = DownloadService(
                    config,
                    jobs,
                    MediaTools(config),
                    adapters=[_FakePlaylistAdapter()],
                )
                snapshot = service.start_playlist(
                    "https://www.youtube.com/playlist?list=PL1234567890",
                    callback=lambda event, payload: events.append(
                        (event, payload)
                    ),
                )
                completed = jobs.wait(snapshot.id, timeout=3)

            self.assertEqual(completed.status, JobState.COMPLETED)
            between_items = [
                payload
                for event, payload in events
                if event == "playlist.progress"
                and payload.get("playlist_current") == 1
            ]
            self.assertEqual(len(between_items), 1)
            self.assertEqual(between_items[0]["phase"], "downloading")
            self.assertEqual(between_items[0]["status"], "running")


class _FakeExtractor:
    def __init__(self) -> None:
        self.calls = 0

    def extract(self, youtube_url: str) -> Mapping[str, Any]:
        self.calls += 1
        return {
            "audio_url": "https://media.example/audio.m4a?expire=9999",
            "title": "Preview",
            "duration": 10,
            "mime_type": "audio/mp4",
        }


class _FakeUpstream:
    status_code = 206
    headers = {
        "Content-Type": "audio/mp4",
        "Content-Length": "6",
        "Content-Range": "bytes 0-5/100",
        "Accept-Ranges": "bytes",
    }

    def __init__(self) -> None:
        self.closed = False

    def iter_content(self, chunk_size: int):
        yield b"abc"
        yield b"def"

    def close(self):
        self.closed = True


class _FakeSession:
    def __init__(self) -> None:
        self.calls: list[dict[str, Any]] = []
        self.upstream = _FakeUpstream()

    def request(self, method, url, **kwargs):
        self.calls.append({"method": method, "url": url, **kwargs})
        return self.upstream


class StreamingServiceTests(unittest.TestCase):
    def test_cache_is_bounded_and_proxy_forwards_range(self) -> None:
        extractor = _FakeExtractor()
        session = _FakeSession()
        now = [100.0]
        service = StreamingService(
            extractor=extractor,
            session=session,
            cache_ttl=60,
            max_cache_entries=2,
            clock=lambda: now[0],
        )
        source = "https://youtu.be/dQw4w9WgXcQ"
        service.get_stream(source)
        service.get_stream(source)
        self.assertEqual(extractor.calls, 1)

        response = service.build_proxy_response(
            source,
            {"Range": "bytes=0-5"},
        )
        self.assertEqual(response.status_code, 206)
        self.assertEqual(
            session.calls[0]["headers"]["Range"],
            "bytes=0-5",
        )

        async def consume() -> bytes:
            chunks = []
            async for chunk in response.body_iterator:
                chunks.append(chunk)
            return b"".join(chunks)

        self.assertEqual(asyncio.run(consume()), b"abcdef")
        self.assertTrue(session.upstream.closed)


class _RecordingRegistrar:
    def __init__(self) -> None:
        self.calls: list[dict[str, Any]] = []

    def register_media(
        self,
        path: Path,
        metadata: Mapping[str, Any],
    ) -> Mapping[str, str]:
        record = {
            "path": Path(path),
            "metadata": dict(metadata),
        }
        self.calls.append(record)
        return {"id": f"track-{len(self.calls)}"}


class _FakeProcessingMedia(MediaTools):
    def probe(self, path, **kwargs):
        return {"streams": [{"codec_type": "audio", "sample_rate": "44100"}]}

    def run_ffmpeg(self, arguments, **kwargs) -> CommandResult:
        path = Path(arguments[arguments.index("-i") + 1])
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        return CommandResult(tuple(map(str, arguments)), 0, f"SHA256={digest}", "")

    def __init__(
        self,
        config: AppConfig,
        *,
        fail_transcode: bool = False,
    ) -> None:
        super().__init__(config)
        self.fail_transcode = fail_transcode
        self.executable_calls: list[dict[str, Any]] = []
        self.copy_calls: list[tuple[Path, Path, str]] = []
        self.duration_calls: list[tuple[Path, str | None]] = []
        self.transcode_calls: list[dict[str, Any]] = []

    def run_executable(
        self,
        executable,
        arguments,
        **kwargs,
    ) -> CommandResult:
        captured_arguments = tuple(arguments)
        self.executable_calls.append(
            {
                "executable": executable,
                "arguments": captured_arguments,
                **kwargs,
            }
        )
        output_index = captured_arguments.index("--output_dir") + 1
        format_index = captured_arguments.index("--output_format") + 1
        output_directory = Path(captured_arguments[output_index])
        output_format = str(captured_arguments[format_index]).lower()
        extension = f".{output_format}"
        nested = output_directory / "generated"
        nested.mkdir(parents=True, exist_ok=True)
        (output_directory / f"source_(Vocals){extension}").write_bytes(
            b"vocals"
        )
        (nested / f"source_(Instrumental){extension}").write_bytes(
            b"instrumental"
        )
        line_callback = kwargs.get("line_callback")
        if line_callback is not None:
            line_callback("\x1b[32mLoading model 10%\x1b[0m")
            line_callback("Separating audio 50%")
        command = (
            str(executable),
            *(str(argument) for argument in captured_arguments),
        )
        return CommandResult(command, 0, "", "")

    def copy_file(
        self,
        source: Path,
        destination: Path,
        *,
        context=None,
        chunk_size: int = 1024 * 1024,
    ) -> Path:
        self.copy_calls.append(
            (
                Path(source),
                Path(destination),
                context.job_id if context is not None else "",
            )
        )
        return super().copy_file(
            source,
            destination,
            context=context,
            chunk_size=chunk_size,
        )

    def duration(
        self,
        path: Path,
        *,
        context=None,
        default: float = 0.0,
    ) -> float:
        del default
        self.duration_calls.append(
            (
                Path(path),
                context.job_id if context is not None else None,
            )
        )
        return 123.25

    def transcode_audio(
        self,
        source: Path,
        destination: Path,
        *,
        context=None,
        filters: str | None = None,
        metadata: Mapping[str, str] | None = None,
        progress_callback=None,
    ) -> Path:
        call = {
            "source": Path(source),
            "destination": Path(destination),
            "job_id": context.job_id if context is not None else "",
            "filters": filters,
            "metadata": dict(metadata or {}),
        }
        self.transcode_calls.append(call)
        destination = Path(destination)
        destination.write_bytes(b"partial" if self.fail_transcode else b"protected")
        if progress_callback is not None:
            progress_callback("out_time_us=61625000")
            progress_callback("progress=end")
        if self.fail_transcode:
            raise RuntimeError("fake transcode failed")
        return destination


class ProcessingServiceTests(unittest.TestCase):
    def test_config_prefers_managed_model_then_bundled_fallback(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            config = _config(Path(temporary))
            bundled = config.resource_dir / "models" / DEFAULT_SEPARATOR_MODEL
            bundled.parent.mkdir(parents=True)
            bundled.write_bytes(b"bundled")
            self.assertEqual(config.separator_model_path, bundled)

            managed = config.model_dir / DEFAULT_SEPARATOR_MODEL
            managed.parent.mkdir(parents=True)
            managed.write_bytes(b"managed")
            self.assertEqual(config.separator_model_path, managed)

    def test_fingerprint_profile_is_deterministic_and_bounded(self) -> None:
        first = ProcessingService.fingerprint_profile("job-id")
        second = ProcessingService.fingerprint_profile("job-id")
        self.assertEqual(first, second)
        _filters, profile = first
        self.assertGreaterEqual(profile["rate_hz"], 40_000)
        self.assertLessEqual(profile["rate_hz"], 50_000)

    def test_chipmunk_profile_clamps_and_keeps_requested_speed(self) -> None:
        filters, profile = ProcessingService.chipmunk_profile(12, 1, 0)
        self.assertIn("asetrate=88200", filters)
        self.assertIn("atempo=0.500000", filters)
        self.assertNotIn("treble", filters)
        self.assertEqual(profile["pitch_semitones"], 12)
        filters, profile = ProcessingService.chipmunk_profile(99, 0.1, 50)
        self.assertEqual(profile, {
            "pitch_semitones": 12,
            "speed": 0.5,
            "brightness_db": 12,
        })
        # 0.5 / 2 needs two ffmpeg-safe atempo steps.
        self.assertEqual(filters.count("atempo=0.500000"), 2)

    def test_chipmunk_publishes_into_ardillas(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            config = _config(Path(temporary))
            config.ensure_directories()
            config.ensure_feature_directories()
            source = config.library_dir / "Song.mp3"
            source.write_bytes(b"source audio")
            media = _FakeProcessingMedia(config)
            registrar = _RecordingRegistrar()
            with JobManager(max_workers=1, max_active_jobs=1) as jobs:
                service = ProcessingService(
                    config, jobs, media, registrar=registrar
                )
                submitted = service.start_chipmunk(
                    source, pitch_semitones=7, speed=1.25, title="Song"
                )
                completed = jobs.wait(submitted.id, timeout=3)
                self.assertEqual(completed.status, JobState.COMPLETED)
                self.assertEqual(
                    [job.id for job in service.list_chipmunk_jobs()],
                    [submitted.id],
                )
            transcode = media.transcode_calls[0]
            self.assertIn("asetrate=", transcode["filters"])
            output = completed.result
            self.assertEqual(Path(output["local_path"]).parent, config.chipmunk_dir)
            self.assertEqual(output["filename"], "Song - Ardilla.mp3")
            self.assertEqual(output["title"], "Song (Ardilla)")
            self.assertEqual(output["playlist"], "Ardillas")
            self.assertEqual(output["chipmunk_profile"]["speed"], 1.25)
            self.assertEqual(len(registrar.calls), 1)

    def test_stem_separation_publishes_registers_and_reports_completed_job(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            config = _config(Path(temporary))
            config.ensure_directories()
            config.ensure_feature_directories()
            config.resource_dir.mkdir(parents=True)
            config.separator_model_path.write_bytes(b"model")
            source = config.library_dir / "Source Song.mp3"
            source.write_bytes(b"source audio")
            media = _FakeProcessingMedia(config)
            registrar = _RecordingRegistrar()
            service_events: list[tuple[str, Mapping[str, Any]]] = []
            callback_events: list[tuple[str, Mapping[str, Any]]] = []

            with JobManager(max_workers=1, max_active_jobs=1) as jobs:
                service = ProcessingService(
                    config,
                    jobs,
                    media,
                    registrar=registrar,
                    events=lambda event, payload: service_events.append(
                        (event, payload)
                    ),
                )
                submitted = service.start_stem_separation(
                    source,
                    output_format="MP3",
                    title="Custom Title",
                    callback=lambda event, payload: callback_events.append(
                        (event, payload)
                    ),
                )
                completed = jobs.wait(submitted.id, timeout=3)

                self.assertEqual(completed.status, JobState.COMPLETED)
                self.assertEqual(service.get(submitted.id), completed)
                self.assertEqual(
                    [job.id for job in service.list_stem_jobs()],
                    [submitted.id],
                )

            self.assertEqual(len(media.executable_calls), 1)
            execution = media.executable_calls[0]
            arguments = execution["arguments"]
            self.assertEqual(execution["executable"], config.separator_path)
            self.assertIsNone(execution["timeout"])
            self.assertEqual(execution["cwd"], config.resource_dir)
            self.assertEqual(
                Path(arguments[arguments.index("--model_file_dir") + 1]),
                config.separator_model_path.parent,
            )
            self.assertEqual(
                arguments[arguments.index("--model_filename") + 1],
                config.separator_model_path.name,
            )
            self.assertEqual(
                arguments[arguments.index("--output_format") + 1],
                "MP3",
            )
            self.assertEqual(
                arguments[arguments.index("--sample_rate") + 1],
                "44100",
            )
            self.assertEqual(
                arguments[arguments.index("--output_bitrate") + 1],
                "320k",
            )
            self.assertEqual(Path(arguments[-1]), source.resolve())
            self.assertEqual(
                execution["env"]["AUDIO_SEPARATOR_MODEL_DIR"],
                str(config.separator_model_path.parent),
            )
            self.assertEqual(execution["env"]["PYTHONUNBUFFERED"], "1")
            self.assertEqual(
                execution["env"]["PATH"].split(os.pathsep)[0],
                str(Path(config.ffmpeg_path).parent),
            )

            outputs = completed.result["outputs"]
            self.assertEqual(
                {output["stem"] for output in outputs},
                {"vocals", "instrumental"},
            )
            self.assertEqual(
                {output["filename"] for output in outputs},
                {
                    "Source Song - Vocals.mp3",
                    "Source Song - Instrumental.mp3",
                },
            )
            self.assertEqual(
                {output["track_id"] for output in outputs},
                {"track-1", "track-2"},
            )
            self.assertTrue(
                all(
                    output["path"].startswith("/files/Separaciones/")
                    for output in outputs
                )
            )
            self.assertEqual(
                {
                    Path(output["local_path"]).read_bytes()
                    for output in outputs
                },
                {b"vocals", b"instrumental"},
            )
            self.assertEqual(len(media.copy_calls), 2)
            self.assertTrue(
                all(call[2] == submitted.id for call in media.copy_calls)
            )

            self.assertEqual(len(registrar.calls), 2)
            registration_by_stem = {
                call["metadata"]["stem"]: call
                for call in registrar.calls
            }
            self.assertEqual(
                registration_by_stem["vocals"]["metadata"]["title"],
                "Custom Title - Vocals",
            )
            self.assertEqual(
                registration_by_stem["instrumental"]["metadata"]["title"],
                "Custom Title - Instrumental",
            )
            for call in registrar.calls:
                self.assertTrue(call["path"].is_file())
                self.assertEqual(call["metadata"]["duration"], 123.25)
                self.assertEqual(call["metadata"]["quality"], "MP3")
                self.assertEqual(call["metadata"]["playlist"], "Separaciones")
                self.assertEqual(call["metadata"]["source_file"], source.name)

            for recorded in (service_events, callback_events):
                names = [event for event, _payload in recorded]
                self.assertIn("processing.stems.queued", names)
                self.assertIn("processing.stems.progress", names)
                self.assertIn("processing.stems.ready", names)
                ready = next(
                    payload
                    for event, payload in recorded
                    if event == "processing.stems.ready"
                )
                self.assertEqual(ready["phase"], "completed")
                self.assertEqual(len(ready["outputs"]), 2)

            self.assertEqual(completed.metadata["phase"], "completed")
            self.assertEqual(len(completed.metadata["outputs"]), 2)
            working_directory = (
                config.cache_dir / "Processing" / "Stems" / submitted.id
            )
            self.assertFalse(working_directory.exists())
            self.assertEqual(
                list(config.stems_dir.glob(".*.publishing*")),
                [],
            )

    def test_stem_separation_supports_entry_point_arguments_and_short_inputs(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            config = dataclasses.replace(
                _config(Path(temporary)),
                separator_args=("launcher.py", "--audio-separator"),
            )
            config.ensure_directories()
            config.ensure_feature_directories()
            config.resource_dir.mkdir(parents=True)
            config.separator_model_path.write_bytes(b"model")
            source = config.library_dir / f"{'Very Long Title ' * 8}.mp3"
            source.write_bytes(b"source audio")
            media = _FakeProcessingMedia(config)

            with (
                mock.patch.object(processing_module, "_SHORT_SEPARATOR_INPUT", True),
                JobManager(max_workers=1, max_active_jobs=1) as jobs,
            ):
                service = ProcessingService(
                    config,
                    jobs,
                    media,
                    registrar=_RecordingRegistrar(),
                )
                submitted = service.start_stem_separation(source, output_format="WAV")
                completed = jobs.wait(submitted.id, timeout=3)

            self.assertEqual(completed.status, JobState.COMPLETED)
            arguments = media.executable_calls[0]["arguments"]
            self.assertEqual(
                arguments[:3],
                ("launcher.py", "--audio-separator", "--model_filename"),
            )
            staged = Path(arguments[-1])
            self.assertEqual(staged.name, f"{submitted.id}-input.mp3")
            self.assertEqual(staged.parent, config.cache_dir / "Processing" / "Stems")
            self.assertEqual(media.copy_calls[0][:2], (source.resolve(), staged))
            self.assertFalse(staged.exists())
            self.assertEqual(
                {output["stem"] for output in completed.result["outputs"]},
                {"vocals", "instrumental"},
            )

    def test_fingerprint_transcodes_publishes_metadata_and_events(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            config = _config(Path(temporary))
            config.ensure_directories()
            config.ensure_feature_directories()
            source = config.library_dir / "Original.flac"
            source.write_bytes(b"source audio")
            media = _FakeProcessingMedia(config)
            registrar = _RecordingRegistrar()
            service_events: list[tuple[str, Mapping[str, Any]]] = []
            callback_events: list[tuple[str, Mapping[str, Any]]] = []

            with JobManager(max_workers=1, max_active_jobs=1) as jobs:
                service = ProcessingService(
                    config,
                    jobs,
                    media,
                    registrar=registrar,
                    events=lambda event, payload: service_events.append(
                        (event, payload)
                    ),
                )
                submitted = service.start_fingerprint(
                    source,
                    title="Original Title",
                    callback=lambda event, payload: callback_events.append(
                        (event, payload)
                    ),
                )
                completed = jobs.wait(submitted.id, timeout=3)

                self.assertEqual(completed.status, JobState.COMPLETED)
                self.assertEqual(
                    [job.id for job in service.list_fingerprint_jobs()],
                    [submitted.id],
                )

            self.assertEqual(
                media.duration_calls,
                [(source.resolve(), submitted.id),
                 (media.transcode_calls[0]["destination"], submitted.id)],
            )
            self.assertEqual(len(media.transcode_calls), 1)
            transcode = media.transcode_calls[0]
            expected_filters, expected_profile = (
                ProcessingService.fingerprint_profile(submitted.id)
            )
            self.assertEqual(transcode["source"], source.resolve())
            self.assertEqual(transcode["job_id"], submitted.id)
            self.assertEqual(transcode["filters"], expected_filters)
            self.assertEqual(
                transcode["metadata"]["ytu_fingerprint_id"],
                submitted.id,
            )
            self.assertIn(
                submitted.id,
                transcode["metadata"]["comment"],
            )

            output = completed.result
            output_path = Path(output["local_path"])
            self.assertEqual(output_path.read_bytes(), b"protected")
            self.assertEqual(output_path.parent, config.protected_dir)
            self.assertEqual(output["filename"], "Original - Protected.flac")
            self.assertEqual(output["path"], "/files/Protegidos/Original%20-%20Protected.flac")
            self.assertEqual(output["track_id"], "track-1")
            self.assertEqual(output["duration"], 123.25)
            self.assertEqual(output["quality"], "FLAC")
            self.assertEqual(output["playlist"], "Protegidos")
            self.assertEqual(output["source_file"], source.name)
            self.assertEqual(output["fingerprint_id"], submitted.id)
            self.assertEqual(output["fingerprint_profile"], expected_profile)
            self.assertEqual(completed.metadata["output"], output)
            self.assertEqual(completed.metadata["phase"], "completed")

            self.assertEqual(len(registrar.calls), 1)
            registered = registrar.calls[0]
            self.assertEqual(registered["path"], output_path)
            self.assertEqual(
                registered["metadata"]["fingerprint_id"],
                submitted.id,
            )
            self.assertEqual(
                registered["metadata"]["fingerprint_profile"],
                expected_profile,
            )

            for recorded in (service_events, callback_events):
                names = [event for event, _payload in recorded]
                self.assertIn("processing.fingerprint.queued", names)
                self.assertIn("processing.fingerprint.progress", names)
                self.assertIn("processing.fingerprint.ready", names)
                progress_values = [
                    payload["progress"]
                    for event, payload in recorded
                    if event == "processing.fingerprint.progress"
                ]
                self.assertTrue(
                    any(45 <= progress <= 55 for progress in progress_values)
                )
                ready = next(
                    payload
                    for event, payload in recorded
                    if event == "processing.fingerprint.ready"
                )
                self.assertEqual(ready["output"]["track_id"], "track-1")

            self.assertFalse(transcode["destination"].exists())
            self.assertEqual(
                list(config.protected_dir.glob(".*.publishing*")),
                [],
            )

    def test_fingerprint_failure_removes_reserved_and_partial_outputs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            config = _config(Path(temporary))
            config.ensure_directories()
            config.ensure_feature_directories()
            source = config.library_dir / "Broken.wav"
            source.write_bytes(b"source audio")
            media = _FakeProcessingMedia(config, fail_transcode=True)
            registrar = _RecordingRegistrar()
            events: list[tuple[str, Mapping[str, Any]]] = []

            with JobManager(max_workers=1, max_active_jobs=1) as jobs:
                service = ProcessingService(
                    config,
                    jobs,
                    media,
                    registrar=registrar,
                    events=lambda event, payload: events.append(
                        (event, payload)
                    ),
                )
                submitted = service.start_fingerprint(source)
                failed = jobs.wait(submitted.id, timeout=3)

            self.assertEqual(failed.status, JobState.FAILED)
            self.assertIn("fake transcode failed", failed.error)
            self.assertEqual(registrar.calls, [])
            self.assertEqual(list(config.protected_dir.iterdir()), [])
            self.assertNotIn(
                "processing.fingerprint.ready",
                [event for event, _payload in events],
            )
            self.assertIn(
                "processing.fingerprint.progress",
                [event for event, _payload in events],
            )


if __name__ == "__main__":
    unittest.main()
