from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

from test_media import _config
from ytultimate.jobs import JobCancelled, JobManager, JobState
from ytultimate.services.downloads import (
    DownloadArtifact,
    DownloadService,
    PlaylistEntry,
    PlaylistInfo,
    PytubeDownloadAdapter,
    YtDlpDownloadAdapter,
    _find_download_output,
)
from ytultimate.services.media import MediaTools
from ytultimate.services.youtube import YoutubeAccessPolicy, YoutubeFailure


URL = "https://www.youtube.com/watch?v=abcdefghijk"


class Context:
    cancelled = False

    def raise_if_cancelled(self):
        if self.cancelled:
            raise JobCancelled("cancelled")


class DownloadRecoveryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.config = _config(self.root)
        self.config.ensure_directories()

    def adapter(self, outcomes):
        options_seen = []

        class Downloader:
            def __init__(self, options):
                self.options = dict(options)
                options_seen.append(self.options)

            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def extract_info(self, _url, *, download):
                path = Path(self.options["outtmpl"].replace("%(ext)s", "m4a"))
                path.write_bytes(b"a different representation")
                outcome = outcomes.pop(0)
                if isinstance(outcome, Exception):
                    raise outcome
                return {"title": "Fixture"}

        adapter = YtDlpDownloadAdapter(
            self.config,
            module=SimpleNamespace(YoutubeDL=Downloader),
            policy=YoutubeAccessPolicy(self.config, {"download_retries": 0}),
        )
        return adapter, options_seen

    def test_forbidden_reextracts_once_without_reusing_previous_representation(self):
        adapter, seen = self.adapter([RuntimeError("HTTP Error 403: Forbidden"), None])
        events = []
        artifact = adapter.download(
            URL, self.root / "staging", media_type="audio", quality="best",
            context=Context(), progress=events.append,
        )
        self.assertEqual(len(seen), 2)
        self.assertNotEqual(seen[0]["outtmpl"], seen[1]["outtmpl"])
        self.assertFalse((self.root / "staging" / "attempt-1").exists())
        self.assertTrue(artifact.path.is_file())
        self.assertEqual(events[0]["phase"], "retrying")
        self.assertFalse(seen[0]["skip_unavailable_fragments"])

    def test_persistent_forbidden_is_bounded_and_has_safe_message(self):
        error = RuntimeError("HTTP 403 https://media.invalid/x?token=private-secret")
        adapter, seen = self.adapter([error, error])
        with self.assertRaises(YoutubeFailure) as caught:
            adapter.download(URL, self.root / "staging", media_type="audio", quality="best",
                             context=Context(), progress=lambda _: None)
        self.assertEqual(len(seen), 2)
        self.assertEqual(caught.exception.code, "forbidden")
        self.assertNotIn("private-secret", str(caught.exception))

    def test_bot_detection_does_not_reextract_same_session(self):
        adapter, seen = self.adapter([RuntimeError("This request was detected as a bot.")])
        with self.assertRaises(YoutubeFailure) as caught:
            adapter.download(URL, self.root / "staging", media_type="audio", quality="best",
                             context=Context(), progress=lambda _: None)
        self.assertEqual(len(seen), 1)
        self.assertEqual(caught.exception.code, "bot_detected")

    def test_cancelled_does_not_retry(self):
        adapter, seen = self.adapter([JobCancelled("cancelled")])
        with self.assertRaises(JobCancelled):
            adapter.download(URL, self.root / "staging", media_type="audio", quality="best",
                             context=Context(), progress=lambda _: None)
        self.assertEqual(len(seen), 1)

    def test_incomplete_and_unmerged_outputs_are_never_published(self):
        (self.root / "source.m4a").touch()
        (self.root / "source.f137.mp4").write_bytes(b"video only")
        (self.root / "source.m4a.part").write_bytes(b"partial")
        self.assertIsNone(_find_download_output(self.root))

    def test_pytube_does_not_exceed_requested_resolution(self):
        class Streams(list):
            def filter(self, **_kwargs): return self
            def order_by(self, _key): return self
            def desc(self): return self

        video = SimpleNamespace(streams=Streams([SimpleNamespace(resolution="1080p")]))
        self.assertIsNone(PytubeDownloadAdapter._select_progressive_stream(video, "720"))

    def test_rate_limit_stops_fallback_and_playlist_after_preserving_completed_items(self):
        class Adapter:
            name = "fixture"
            calls = 0

            def playlist_info(self, _url, *, limit):
                return PlaylistInfo("Fixture", "", "", tuple(
                    PlaylistEntry(str(i), URL, str(i)) for i in range(4)
                ))

            def download(self, _url, directory, **_kwargs):
                self.calls += 1
                if self.calls > 1:
                    raise RuntimeError("HTTP Error 429: Too Many Requests")
                path = directory / "source.mp3"
                path.write_bytes(b"complete")
                return DownloadArtifact(path, "Fixture", engine=self.name)

        class Fallback:
            name = "fallback"
            calls = 0

            def download(self, *_args, **_kwargs):
                self.calls += 1
                raise AssertionError("rate limits must not trigger fallback")

        adapter, fallback = Adapter(), Fallback()
        with JobManager(max_workers=1, max_active_jobs=2) as jobs:
            service = DownloadService(self.config, jobs, MediaTools(self.config),
                                      adapters=[adapter, fallback])
            snapshot = service.start_playlist("https://www.youtube.com/playlist?list=PLfixture12345")
            done = jobs.wait(snapshot.id, timeout=3)
        self.assertEqual(done.status, JobState.COMPLETED)
        self.assertEqual(done.result["completed"], 1)
        self.assertEqual(done.result["failed"], 1)
        self.assertEqual(done.result["skipped"], 2)
        self.assertTrue(done.result["stopped"])
        self.assertEqual(adapter.calls, 2)
        self.assertEqual(fallback.calls, 0)


if __name__ == "__main__":
    unittest.main()
