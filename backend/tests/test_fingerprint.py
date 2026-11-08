from __future__ import annotations

import array
import dataclasses
import hashlib
import math
import os
import random
import shutil
import sys
import tempfile
import unittest
import wave
from pathlib import Path
from unittest import mock

from test_media import _config, _FakeProcessingMedia, _RecordingRegistrar
from ytultimate.jobs import JobCancelled, JobManager, JobState
from ytultimate.services.fingerprint import (
    acoustic_fingerprint,
    compare_fingerprint_frames,
    verify_audio_change,
)
from ytultimate.services.media import CommandExecutionError, CommandResult, MediaToolError, MediaTools
from ytultimate.services.processing import ProcessingService
from ytultimate.schemas import FingerprintRequest


class FingerprintVerificationTests(unittest.TestCase):
    def test_intensity_validation_and_preserved_sample_rates(self):
        self.assertEqual(FingerprintRequest().intensity, "strong")
        with self.assertRaises(ValueError):
            FingerprintRequest(intensity="invalid")
        for rate in (22050, 44100, 48000, 96000):
            _, balanced = ProcessingService.fingerprint_profile("fixed", "balanced", rate)
            _, strong = ProcessingService.fingerprint_profile("fixed", "strong", rate)
            self.assertGreater(strong["pitch_semitones"], balanced["pitch_semitones"])
            self.assertEqual(strong["speed"], balanced["speed"])
            self.assertEqual(strong["sample_rate"], rate)
            self.assertEqual(strong["output_format"], "FLAC")
        with self.assertRaises(ValueError):
            ProcessingService.fingerprint_profile("fixed", "invalid")

    def setUp(self):
        self.media = mock.Mock(spec=MediaTools)
        self.media.config = mock.Mock(fpcalc_path="fpcalc")
        self.context = mock.Mock()
        self.hashes = mock.patch(
            "ytultimate.services.fingerprint.pcm_digest", side_effect=["source", "output"],
        )
        self.hashes.start()
        self.addCleanup(self.hashes.stop)

    def compare(self, before, after):
        with mock.patch("ytultimate.services.fingerprint.acoustic_fingerprint", side_effect=[before, after]):
            return verify_audio_change(self.media, Path("source"), Path("output"), self.context)

    def test_length_change_alone_does_not_claim_changed_acoustic_features(self):
        result = self.compare([123] * 30, [123] * 25)
        self.assertEqual(result["status"], "unchanged")
        self.assertFalse(result["recognition_tested"])
        self.assertEqual(result["local_comparison"]["aligned"]["compared_frames"], 25)
        self.assertEqual(result["local_comparison"]["best_offset"]["offset_frames"], 0)
        self.assertEqual(result["local_comparison"]["aligned"]["bit_similarity"], 1)

    def test_changed_and_insufficient_fingerprints_are_distinguished(self):
        self.assertEqual(self.compare([123] * 30, [124] * 30)["status"], "changed")

    def test_short_audio_is_not_verified(self):
        result = self.compare([123] * 5, [124] * 5)
        self.assertEqual(result["status"], "insufficient_audio")
        self.assertNotIn("local_comparison", result)

    def test_missing_tool_does_not_claim_verification(self):
        self.media.config.fpcalc_path = ""
        result = verify_audio_change(self.media, Path("source"), Path("output"), self.context)
        self.assertEqual(result["status"], "unavailable")
        self.assertNotIn("local_comparison", result)

    def test_shifted_fingerprint_retains_actual_difference_and_reports_local_agreement(self):
        generator = random.Random(2026)
        before = [generator.getrandbits(32) for _ in range(80)]
        after = [generator.getrandbits(32) for _ in range(5)] + before
        result = self.compare(before, after)
        self.assertEqual(result["status"], "changed")
        self.assertFalse(result["recognition_tested"])
        comparison = result["local_comparison"]
        self.assertLess(comparison["aligned"]["bit_similarity"], 1)
        self.assertEqual(comparison["best_offset"]["bit_similarity"], 1)
        self.assertEqual(comparison["best_offset"]["offset_frames"], 5)

    def test_empty_fingerprint_is_supported_but_other_failures_propagate(self):
        self.media.run_executable.side_effect = CommandExecutionError(
            "empty", command=["fpcalc"], returncode=2, stderr="ERROR: Empty fingerprint\n",
        )
        self.assertEqual(acoustic_fingerprint(self.media, Path("short.wav"), self.context), [])
        self.media.run_executable.side_effect = CommandExecutionError(
            "broken", command=["fpcalc"], returncode=2, stderr="ERROR: Invalid input",
        )
        with self.assertRaises(CommandExecutionError):
            acoustic_fingerprint(self.media, Path("broken.wav"), self.context)

    def test_invalid_verifier_output_is_rejected(self):
        for payload in ("bad json", '{"fingerprint": [true]}', '{"fingerprint": [-1]}', '[]'):
            with self.subTest(payload=payload):
                self.media.run_executable.return_value = CommandResult((), 0, payload, "")
                with self.assertRaises(MediaToolError):
                    acoustic_fingerprint(self.media, Path("source.wav"), self.context)

    def test_verifier_failure_and_cancellation_remove_partial_outputs(self):
        for error, state in ((MediaToolError("verification failed"), JobState.FAILED),
                             (JobCancelled("canceled"), JobState.CANCELLED)):
            with self.subTest(state=state), tempfile.TemporaryDirectory() as temporary:
                config = _config(Path(temporary))
                config.ensure_directories()
                source = config.library_dir / "source.wav"
                source.write_bytes(b"source")
                registrar = _RecordingRegistrar()
                with JobManager(max_workers=1) as jobs:
                    service = ProcessingService(config, jobs, _FakeProcessingMedia(config), registrar=registrar)
                    with mock.patch("ytultimate.services.processing.verify_audio_change", side_effect=error):
                        job = jobs.wait(service.start_fingerprint(source).id, timeout=3)
                self.assertEqual(job.status, state)
                self.assertEqual(list(config.protected_dir.iterdir()), [])
                self.assertEqual(registrar.calls, [])
                self.assertEqual(source.read_bytes(), b"source")


class FingerprintComparisonTests(unittest.TestCase):
    def test_exact_bit_and_frame_agreement_are_distinct(self):
        before = [0] * 32
        after = before.copy()
        after[16] = 1
        comparison = compare_fingerprint_frames(before, after)
        aligned = comparison["aligned"]
        self.assertEqual(aligned["compared_frames"], 32)
        self.assertEqual(aligned["bit_similarity"], 1 - 1 / (32 * 32))
        self.assertEqual(aligned["exact_frame_fraction"], 31 / 32)
        self.assertEqual(comparison["metric"], "chromaprint_bit_agreement_v1")

    def test_complementary_frames_have_zero_bit_agreement(self):
        comparison = compare_fingerprint_frames([0] * 40, [0xFFFFFFFF] * 40)
        for result in (comparison["aligned"], comparison["best_offset"]):
            self.assertEqual(result["bit_similarity"], 0)
            self.assertEqual(result["exact_frame_fraction"], 0)
        self.assertEqual(comparison["best_offset"]["offset_frames"], 0)

    def test_leading_offsets_in_either_direction_find_the_same_frames(self):
        generator = random.Random(123)
        frames = [generator.getrandbits(32) for _ in range(100)]
        prefix = [generator.getrandbits(32) for _ in range(6)]
        for source, output, expected_offset in (
            (frames, prefix + frames, 6), (prefix + frames, frames, -6),
        ):
            with self.subTest(expected_offset=expected_offset):
                comparison = compare_fingerprint_frames(source, output)
                self.assertEqual(comparison["best_offset"], {
                    "offset_frames": expected_offset, "compared_frames": 100,
                    "bit_similarity": 1, "exact_frame_fraction": 1,
                })
                self.assertEqual(comparison["offset_convention"], "output_index_minus_source_index")

    def test_offset_search_is_bounded_and_does_not_select_a_tiny_matching_tail(self):
        comparison = compare_fingerprint_frames([0] * 50, [0xFFFFFFFF] * 40 + [0] * 10)
        best = comparison["best_offset"]
        self.assertEqual(comparison["min_compared_frames"], 40)
        self.assertGreaterEqual(best["compared_frames"], 40)
        self.assertLessEqual(abs(best["offset_frames"]), 16)
        self.assertEqual(best["bit_similarity"], 0.25)
        generator = random.Random(456)
        frames = [generator.getrandbits(32) for _ in range(100)]
        outside_window = [generator.getrandbits(32) for _ in range(17)] + frames
        result = compare_fingerprint_frames(frames, outside_window)
        self.assertLess(result["best_offset"]["bit_similarity"], 1)
        self.assertLessEqual(abs(result["best_offset"]["offset_frames"]), 16)

    def test_insufficient_frames_are_not_compared_and_minimum_sample_is_unshifted(self):
        for length in (0, 1, 15):
            with self.subTest(length=length), self.assertRaises(ValueError):
                compare_fingerprint_frames([1] * length, [1] * 30)
        comparison = compare_fingerprint_frames([1] * 16, [1] * 16)
        self.assertEqual(comparison["best_offset"], comparison["aligned"])
        self.assertEqual(comparison["best_offset"]["compared_frames"], 16)


ROOT = Path(__file__).resolve().parents[2]
NATIVE = ROOT / "frontend" / (".electron-bin-win" if os.name == "nt" else ".electron-bin")
SUFFIX = ".exe" if os.name == "nt" else ""


def tool(name):
    configured = os.environ.get(f"YTU_{name.upper()}_PATH")
    candidates = [configured, str(NATIVE / f"{name}{SUFFIX}"), shutil.which(name)]
    return next((item for item in candidates if item and Path(item).is_file()), "")


@unittest.skipUnless(tool("ffmpeg") and tool("ffprobe") and tool("fpcalc"), "Native audio tools required")
class FingerprintAudioIntegrationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.config = dataclasses.replace(
            _config(Path(self.temporary.name)), ffmpeg_path=tool("ffmpeg"),
            ffprobe_path=tool("ffprobe"), fpcalc_path=tool("fpcalc"),
        )
        self.config.ensure_directories()
        self.media = MediaTools(self.config)

    def fixture(self, *, seconds=18, tone=False, rate=22050):
        path = self.config.library_dir / "Synthetic.wav"
        samples = array.array("h")
        notes = [220, 261.63, 329.63, 293.66, 392, 349.23, 246.94, 440]
        for index in range(int(rate * seconds)):
            t = index / rate
            note = 440 if tone else notes[int(t * 3) % len(notes)]
            envelope = 1 if tone else 0.4 + 0.6 * math.exp(-6 * (t * 3 % 1))
            left = 0.3 * envelope * math.sin(2 * math.pi * note * t)
            right = left if tone else 0.2 * math.sin(2 * math.pi * note * 1.5 * t)
            samples.extend([round(32767 * left), round(32767 * right)])
        if sys.byteorder != "little":
            samples.byteswap()
        with wave.open(str(path), "wb") as output:
            output.setparams((2, 2, rate, 0, "NONE", "not compressed"))
            output.writeframes(samples.tobytes())
        return path

    def process(self, source, intensity="strong"):
        original = hashlib.sha256(source.read_bytes()).hexdigest()
        with JobManager(max_workers=1) as jobs:
            service = ProcessingService(self.config, jobs, self.media)
            job = jobs.wait(service.start_fingerprint(source, intensity=intensity).id, timeout=60)
        self.assertEqual(job.status, JobState.COMPLETED, job.error)
        self.assertEqual(hashlib.sha256(source.read_bytes()).hexdigest(), original)
        return job.result

    def test_real_chromaprint_and_all_supported_audio_formats(self):
        wave_source = self.fixture()
        for extension in (".wav", ".flac", ".mp3", ".m4a", ".aac", ".ogg", ".opus"):
            with self.subTest(extension=extension):
                source = wave_source
                if extension != ".wav":
                    source = self.config.library_dir / f"Synthetic{extension}"
                    self.media.transcode_audio(wave_source, source)
                result = self.process(source)
                verification = result["fingerprint_verification"]
                self.assertEqual(verification["status"], "changed")
                self.assertNotEqual(verification["source_pcm_sha256"], verification["output_pcm_sha256"])
                self.assertNotEqual(verification["source_fingerprint_sha256"], verification["output_fingerprint_sha256"])
                # Raw AAC's duration is estimated from bitrate and can be off
                # by hundreds of milliseconds. Measure decoded samples instead.
                source_decoded = self.config.cache_dir / "source-decoded.wav"
                self.media.transcode_audio(source, source_decoded)
                with wave.open(str(source_decoded), "rb") as decoded:
                    source_duration = decoded.getnframes() / decoded.getframerate()
                expected_duration = source_duration / result["fingerprint_profile"]["speed"]
                self.assertAlmostEqual(result["duration"], expected_duration, delta=0.2)
                info = self.media.probe(Path(result["local_path"]))
                self.assertEqual(info["streams"][0]["channels"], 2)
                self.assertEqual(info["streams"][0]["codec_name"], "flac")
                self.assertEqual(int(info["streams"][0]["bits_per_raw_sample"]), 24)
                source_info = self.media.probe(source)
                self.assertEqual(info["streams"][0]["sample_rate"], source_info["streams"][0]["sample_rate"])

    def test_pitch_changes_samples_and_short_clip_is_not_acoustically_verified(self):
        result = self.process(self.fixture(seconds=2, tone=True))
        self.assertEqual(result["fingerprint_verification"]["status"], "insufficient_audio")
        decoded = self.config.library_dir / "decoded.wav"
        self.media.run_ffmpeg(["-y", "-i", result["local_path"], "-c:a", "pcm_s16le", decoded])
        with wave.open(str(decoded), "rb") as output:
            samples = array.array("h", output.readframes(output.getnframes()))
            rate = output.getframerate()
        if sys.byteorder != "little":
            samples.byteswap()
        # Count positive zero crossings over a central second, independently of
        # FFmpeg metadata and the filter string.
        channel = samples[2 * (rate // 3):2 * (rate // 3 + rate):2]
        measured_hz = sum(a <= 0 < b for a, b in zip(channel, channel[1:]))
        expected_hz = 440 * 2 ** (result["fingerprint_profile"]["pitch_semitones"] / 12)
        self.assertAlmostEqual(measured_hz, expected_hz, delta=2)
        self.assertLess(max(abs(value) for value in samples), 32767)

    def test_both_intensities_preserve_high_resolution_and_spectral_change(self):
        source = self.fixture(seconds=12, rate=96000)
        for intensity in ("balanced", "strong"):
            with self.subTest(intensity=intensity):
                result = self.process(source, intensity)
                info = self.media.probe(Path(result["local_path"]))["streams"][0]
                self.assertEqual(info["sample_rate"], "96000")
                self.assertEqual(info["bits_per_raw_sample"], "24")
                self.assertEqual(result["fingerprint_verification"]["status"], "changed")

    def test_metadata_only_edits_are_rejected(self):
        source = self.fixture(seconds=2, tone=True)
        copied = self.config.library_dir / "tags.wav"
        self.media.run_ffmpeg(["-y", "-i", source, "-c:a", "copy", "-metadata", "comment=different", copied])
        with JobManager(max_workers=1) as jobs:
            job = jobs.submit("verify", lambda context: verify_audio_change(self.media, source, copied, context))
            result = jobs.wait(job.id, timeout=10)
        self.assertEqual(result.status, JobState.FAILED)
        self.assertIn("no cambió las muestras", result.error)


if __name__ == "__main__":
    unittest.main()
