from __future__ import annotations

import dataclasses
import hashlib
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import numpy as np
import soundfile as sf

from test_fingerprint import tool
from test_media import _config, _RecordingRegistrar
from ytultimate.jobs import JobCancelled, JobManager, JobState
from ytultimate.schemas import FingerprintRequest
from ytultimate.services.media import CommandResult, MediaToolError, MediaTools
from ytultimate.services.processing import ProcessingError, ProcessingService


class QualityProfileTests(unittest.TestCase):
    def test_quality_declares_gentle_shared_changes_at_model_sample_rate(self):
        self.assertEqual(FingerprintRequest(intensity="quality").intensity, "quality")
        for rate in (22050, 44100, 96000):
            filters, profile = ProcessingService.fingerprint_profile("fixed", "quality", rate)
            self.assertEqual(profile["pitch_semitones"], 0.28)
            self.assertEqual(profile["pitch_range_semitones"], [0.20, 0.36])
            self.assertGreater(profile["speed"], 1)
            self.assertLess(profile["speed"], 1.02)
            self.assertEqual(profile["sample_rate"], 44100)
            self.assertEqual(profile["processing_target"], "instrumental_then_full_mix")
            self.assertNotIn("vocal_gain", profile)
            self.assertFalse(profile["vocals_preserved"])
            self.assertEqual(profile["vocal_processing"], "shared_smooth_remap")
            for unwanted in ("asetrate", "atempo", "alimiter"):
                self.assertNotIn(unwanted, filters)
            self.assertEqual(profile["instrumental_method"], "dynamic_spectral_eq")
            self.assertEqual(profile["max_spectral_gain_db"], 6.0)
            self.assertEqual(profile["version"], 6)
        with self.assertRaises(ValueError):
            ProcessingService.fingerprint_profile("fixed", "quality", 0)

    def test_mixer_scales_only_instrumental_and_preserves_every_vocal_sample(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            vocals = np.full((200_000, 2), 0.25)
            vocals[150_000, 0] = 0.8
            vocals[-1, 1] = -0.8
            music = np.full_like(vocals, 0.5)
            music[-1, 1] = -0.5
            for name, samples in (("v", vocals), ("i", music)):
                sf.write(root / f"{name}.wav", samples, 44100, subtype="DOUBLE")
            gain = ProcessingService._mix_preserving_vocals(
                mock.Mock(), root / "v.wav", root / "i.wav", root / "mixed.wav",
            )
            output, _ = sf.read(root / "mixed.wav", dtype="float64")
            self.assertAlmostEqual(gain, 0.4, places=6)
            np.testing.assert_allclose(output - gain * music, vocals, atol=1e-15, rtol=0)
            self.assertLessEqual(np.max(np.abs(output)), 1)
            self.assertEqual(len(output), len(vocals))

    def test_saturated_vocals_never_silence_the_instrumental(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            vocals = np.full((100, 2), 0.5)
            vocals[-1, 1] = 1.0
            sf.write(root / "v.wav", vocals, 44100, subtype="FLOAT")
            sf.write(root / "i.wav", np.full((100, 2), 0.5), 44100, subtype="FLOAT")
            with self.assertRaisesRegex(ProcessingError, "margen suficiente"):
                ProcessingService._mix_preserving_vocals(
                    mock.Mock(), root / "v.wav", root / "i.wav", root / "mixed.wav",
                )
            self.assertFalse((root / "mixed.wav").exists())

    def test_mixer_rejects_mismatched_stems_and_respects_cancellation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name, frames in (("v", 100), ("i", 99)):
                sf.write(root / f"{name}.wav", np.zeros((frames, 2)), 44100, subtype="FLOAT")
            with self.assertRaises(ProcessingError):
                ProcessingService._mix_preserving_vocals(
                    mock.Mock(), root / "v.wav", root / "i.wav", root / "mixed.wav",
                )
            sf.write(root / "i.wav", np.zeros((100, 2)), 44100, subtype="FLOAT")
            context = mock.Mock()
            context.raise_if_cancelled.side_effect = JobCancelled("canceled")
            with self.assertRaises(JobCancelled):
                ProcessingService._mix_preserving_vocals(
                    context, root / "v.wav", root / "i.wav", root / "mixed.wav",
                )


class _KnownStemsMedia(MediaTools):
    """Only inference is replaced; decoding, EQ, export and verification are real."""

    def __init__(self, config, vocals, instrumental, *, separator_error=None, missing_stem=False):
        super().__init__(config)
        self.vocals = vocals.astype(np.float32)
        self.instrumental = instrumental.astype(np.float32)
        self.separator_error = separator_error
        self.missing_stem = missing_stem
        self.separator_call = None
        self.ffmpeg_calls = []
        self.mixed_samples = None
        self.remapped_samples = None
        self.modified_instrumental = None
        self.prepared_info = None

    def run_ffmpeg(self, arguments, **kwargs):
        self.ffmpeg_calls.append(tuple(map(str, arguments)))
        return super().run_ffmpeg(arguments, **kwargs)

    def run_executable(self, executable, arguments, **kwargs):
        if executable != self.config.separator_path:
            return super().run_executable(executable, arguments, **kwargs)
        self.separator_call = (arguments, kwargs)
        prepared = Path(arguments[-1])
        self.prepared_info = sf.info(prepared)
        if self.separator_error:
            raise self.separator_error
        output_dir = Path(arguments[arguments.index("--output_dir") + 1])
        sf.write(output_dir / "vocals.wav", self.vocals, 44100, subtype="FLOAT")
        if not self.missing_stem:
            sf.write(output_dir / "instrumental.wav", self.instrumental, 44100, subtype="FLOAT")
        kwargs["line_callback"]("Separating audio 50%")
        return CommandResult(tuple(map(str, arguments)), 0, "", "")

    def transcode_audio(self, source, destination, **kwargs):
        if source.name == "mixed.wav":
            self.mixed_samples, _ = sf.read(source)
            self.modified_instrumental, _ = sf.read(source.with_name("instrumental-modified.wav"))
        if source.name == "remapped.wav":
            self.remapped_samples, _ = sf.read(source)
        return super().transcode_audio(source, destination, **kwargs)


@unittest.skipUnless(tool("ffmpeg") and tool("ffprobe") and tool("fpcalc"), "Native audio tools required")
class QualityAudioIntegrationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.config = dataclasses.replace(
            _config(Path(self.temporary.name)), ffmpeg_path=tool("ffmpeg"),
            ffprobe_path=tool("ffprobe"), fpcalc_path=tool("fpcalc"),
            separator_path="test-separator", separator_args=("launcher.py", "--audio-separator"),
        )
        self.config.ensure_directories()
        self.config.resource_dir.mkdir(parents=True, exist_ok=True)
        self.config.separator_model_path.parent.mkdir(parents=True, exist_ok=True)
        self.config.separator_model_path.write_bytes(b"test-model")
        self.registrar = _RecordingRegistrar()

    def fixture(self, seconds=14):
        rate = 44100
        times = np.arange(rate * seconds) / rate
        envelope = 0.4 + 0.6 * np.exp(-6 * ((times * 3) % 1))
        notes = np.asarray([220, 261.63, 329.63, 293.66, 392, 349.23, 246.94, 440])
        note = notes[(times * 3).astype(int) % len(notes)]
        vocals = np.column_stack([
            0.06 * np.sin(2 * np.pi * 440 * times),
            0.06 * np.sin(2 * np.pi * 440 * times + 0.1),
        ])
        vocals[-1] = [0.25, -0.2]
        music = np.column_stack([
            0.24 * envelope * np.sin(2 * np.pi * note * times) + 0.1 * np.sin(2 * np.pi * note * 3 * times),
            0.20 * envelope * np.sin(2 * np.pi * note * 1.5 * times) + 0.1 * np.sin(2 * np.pi * note * 4 * times),
        ])
        source = self.config.library_dir / "Original (Vocals) long title.wav"
        sf.write(source, vocals + music, rate, subtype="PCM_24")
        return source, vocals, music

    def run_quality(self, source, media):
        events = []
        with JobManager(max_workers=1) as jobs:
            service = ProcessingService(
                self.config, jobs, media, registrar=self.registrar,
                events=lambda name, value: events.append((name, value)),
            )
            submitted = service.start_fingerprint(source, intensity="quality")
            completed = jobs.wait(submitted.id, timeout=60)
        return completed, events

    def assert_temporary_stems_cleaned(self):
        self.assertEqual(list(self.config.stems_dir.glob("**/*")), [])
        self.assertEqual(list((self.config.cache_dir / "Processing" / "Quality").glob("**/*.wav")), [])

    def test_real_export_verifies_pre_remap_voice_then_changed_final_mix(self):
        source, vocals, music = self.fixture()
        original_digest = hashlib.sha256(source.read_bytes()).hexdigest()
        media = _KnownStemsMedia(self.config, vocals, music)
        completed, events = self.run_quality(source, media)
        self.assertEqual(completed.status, JobState.COMPLETED, completed.error)
        result = completed.result
        profile = result["fingerprint_profile"]
        output, rate = sf.read(result["local_path"])
        self.assertEqual(rate, 44100)
        self.assertEqual(output.shape[1], vocals.shape[1])
        self.assertLess(len(output), len(vocals))
        self.assertLess(len(vocals) / len(output), 1.02)
        self.assertEqual(sf.info(result["local_path"]).subtype, "PCM_24")
        self.assertNotIn("vocal_preservation_scope", profile)
        self.assertFalse(profile["vocals_preserved"])
        self.assertEqual(profile["source_sample_rate"], 44100)
        self.assertEqual(profile["pre_remap_vocal_integrity"]["status"], "verified")
        self.assertEqual(profile["pre_remap_vocal_integrity"]["scope"], "separated_stem_before_shared_remap")
        self.assertEqual(profile["pre_remap_vocal_integrity"]["frames"], len(vocals))
        self.assertEqual(profile["pre_remap_vocal_integrity"]["channels"], 2)
        self.assertLessEqual(profile["pre_remap_vocal_integrity"]["max_abs_error"], 2 ** -23)
        # The final encoding matches the authorized processed mix. Only the
        # pre-remap sum retains the original separated vocal at unity gain.
        np.testing.assert_allclose(
            output, media.remapped_samples, atol=2 ** -23, rtol=0,
        )
        np.testing.assert_allclose(
            media.mixed_samples - profile["instrumental_gain"] * media.modified_instrumental,
            media.vocals, atol=1e-15, rtol=0,
        )
        self.assertGreater(np.max(np.abs(media.modified_instrumental - media.instrumental)), 0.001)
        self.assertEqual(profile["final_audio_integrity"]["status"], "verified")
        self.assertEqual(profile["final_audio_integrity"]["scope"], "processed_full_mix")
        self.assertLessEqual(profile["final_audio_integrity"]["true_peak_estimate_dbtp"], -1)
        self.assertEqual(profile["shared_processing"]["output_frames"], len(output))
        self.assertEqual(profile["speed"], len(vocals) / len(output))
        self.assertEqual(result["fingerprint_verification"]["status"], "changed")
        self.assertEqual(hashlib.sha256(source.read_bytes()).hexdigest(), original_digest)
        self.assertEqual(len(self.registrar.calls), 1)
        self.assertEqual(self.registrar.calls[0]["path"], Path(result["local_path"]))
        args, kwargs = media.separator_call
        self.assertEqual(tuple(args[:2]), self.config.separator_args)
        self.assertEqual(args[args.index("--normalization") + 1], "1")
        self.assertEqual(args[args.index("--amplification") + 1], "0")
        self.assertIn("--use_soundfile", args)
        self.assertEqual(media.prepared_info.subtype, "FLOAT")
        self.assertEqual(media.prepared_info.samplerate, 44100)
        self.assertEqual(Path(args[-1]).name, "input.wav")
        self.assertEqual(json.loads(args[args.index("--custom_output_names") + 1]), {
            "Vocals": "vocals", "Instrumental": "instrumental",
        })
        self.assertTrue(kwargs["env"]["PATH"].startswith(str(Path(self.config.ffmpeg_path).parent)))
        instrumental_calls = [call for call in media.ffmpeg_calls if "-af" in call]
        self.assertEqual(len(instrumental_calls), 2)
        self.assertEqual(instrumental_calls[0][instrumental_calls[0].index("-af") + 1], "aformat=sample_fmts=s32")
        self.assertEqual(profile["instrumental_processing"]["max_gain_db"], 6.0)
        phases = {event[1]["metadata"].get("phase") for event in events}
        self.assertTrue({"separating", "processing", "recombining", "remapping", "verifying", "completed"}.issubset(phases))
        self.assert_temporary_stems_cleaned()

    def test_final_vocal_audit_rejects_a_changed_mix_without_publishing(self):
        source, vocals, music = self.fixture(seconds=1)

        class AlteredMixMedia(_KnownStemsMedia):
            def transcode_audio(self, source, destination, **kwargs):
                super().transcode_audio(source, destination, **kwargs)
                samples, rate = sf.read(destination)
                samples[100:200] += 0.01
                sf.write(destination, samples, rate, subtype="PCM_24")

        completed, _ = self.run_quality(source, AlteredMixMedia(self.config, vocals, music))
        self.assertEqual(completed.status, JobState.FAILED)
        self.assertIn("alteró la voz", completed.error)
        self.assertEqual(list(self.config.protected_dir.iterdir()), [])
        self.assertEqual(self.registrar.calls, [])
        self.assert_temporary_stems_cleaned()

    def test_final_processed_audio_audit_rejects_a_changed_export(self):
        source, vocals, music = self.fixture(seconds=1)

        class AlteredFinalMedia(_KnownStemsMedia):
            def transcode_audio(self, source, destination, **kwargs):
                super().transcode_audio(source, destination, **kwargs)
                if source.name == "remapped.wav":
                    samples, rate = sf.read(destination)
                    samples[100:200] += 0.01
                    sf.write(destination, samples, rate, subtype="PCM_24")

        completed, _ = self.run_quality(source, AlteredFinalMedia(self.config, vocals, music))
        self.assertEqual(completed.status, JobState.FAILED)
        self.assertIn("alteró la mezcla remapeada", completed.error)
        self.assertEqual(list(self.config.protected_dir.iterdir()), [])
        self.assertEqual(self.registrar.calls, [])
        self.assert_temporary_stems_cleaned()

    def test_failures_and_cancellation_never_publish_partial_or_intermediate_files(self):
        source, vocals, music = self.fixture(seconds=1)
        for kind, error, expected in (
            ("separator", RuntimeError("inference failed"), JobState.FAILED),
            ("separator", JobCancelled("canceled"), JobState.CANCELLED),
            ("verifier", MediaToolError("verification failed"), JobState.FAILED),
            ("verifier", JobCancelled("canceled"), JobState.CANCELLED),
        ):
            with self.subTest(kind=kind, error=error):
                media = _KnownStemsMedia(self.config, vocals, music, separator_error=error if kind == "separator" else None)
                with mock.patch("ytultimate.services.processing.verify_audio_change", side_effect=error):
                    completed, _ = self.run_quality(source, media)
                self.assertEqual(completed.status, expected)
                self.assertEqual(list(self.config.protected_dir.iterdir()), [])
                self.assertEqual(self.registrar.calls, [])
                self.assert_temporary_stems_cleaned()

    def test_missing_stem_fails_explicitly_without_filtering_original_mix(self):
        source, vocals, music = self.fixture(seconds=1)
        media = _KnownStemsMedia(self.config, vocals, music, missing_stem=True)
        completed, _ = self.run_quality(source, media)
        self.assertEqual(completed.status, JobState.FAILED)
        self.assertIn("dos stems", completed.error)
        self.assertEqual(len(media.ffmpeg_calls), 1)
        self.assertEqual(list(self.config.protected_dir.iterdir()), [])
        self.assert_temporary_stems_cleaned()


if __name__ == "__main__":
    unittest.main()
