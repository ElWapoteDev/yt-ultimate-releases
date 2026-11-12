from __future__ import annotations

import hashlib
import tempfile
import unittest
from pathlib import Path

import numpy as np
import soundfile as sf

from ytultimate.jobs import JobCancelled
from ytultimate.services.quality_audio import (
    dynamic_instrumental_eq,
    estimate_audio_true_peak,
    shared_smooth_remap,
)


class _Context:
    def __init__(self, cancel_after=None):
        self.checks = 0
        self.cancel_after = cancel_after

    def raise_if_cancelled(self):
        self.checks += 1
        if self.cancel_after is not None and self.checks >= self.cancel_after:
            raise JobCancelled("canceled")


class DynamicInstrumentalEQTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "instrumental.wav"

    def write(self, samples, rate=44100):
        sf.write(self.source, samples, rate, subtype="DOUBLE")

    def process(self, *, seed="fixture", gain=6, context=None, filename="out.wav"):
        destination = self.root / filename
        metadata = dynamic_instrumental_eq(
            context or _Context(), self.source, destination, seed=seed, max_gain_db=gain,
        )
        samples, rate = sf.read(destination, always_2d=True)
        return samples, rate, metadata

    def test_zero_gain_reconstructs_first_last_and_short_file_samples(self):
        for frames in (1, 17, 511, 512, 2048, 4103):
            with self.subTest(frames=frames):
                samples = np.random.default_rng(frames).uniform(-0.2, 0.2, (frames, 2))
                samples[0] = [0.7, -0.6]
                samples[-1] = [-0.4, 0.5]
                self.write(samples)
                output, rate, metadata = self.process(gain=0)
                self.assertEqual(output.shape, samples.shape)
                self.assertEqual(rate, 44100)
                np.testing.assert_allclose(output, samples, atol=5e-16, rtol=0)
                self.assertEqual(metadata["frames"], frames)

    def test_stereo_proportions_and_sample_count_survive_time_varying_eq(self):
        times = np.arange(44100 * 4 + 71) / 44100
        mono = 0.1 * np.sin(2 * np.pi * 500 * times) + 0.08 * np.sin(2 * np.pi * 2500 * times)
        samples = np.column_stack([mono, -0.5 * mono])
        self.write(samples)
        digest = hashlib.sha256(self.source.read_bytes()).hexdigest()
        output, rate, metadata = self.process()
        self.assertEqual(output.shape, samples.shape)
        self.assertEqual(rate, 44100)
        np.testing.assert_allclose(output[:, 1], -0.5 * output[:, 0], atol=1e-15, rtol=0)
        self.assertGreater(np.sqrt(np.mean((output - samples) ** 2)), 0.01)
        self.assertTrue(np.isfinite(output).all())
        self.assertEqual(hashlib.sha256(self.source.read_bytes()).hexdigest(), digest)
        self.assertEqual(sf.info(self.root / "out.wav").subtype, "DOUBLE")
        self.assertEqual(metadata["band_centers_hz"], [100, 220, 500, 1100, 2500, 5500, 11000])
        self.assertEqual(metadata["max_gain_db"], 6)
        # A bounded spectral gain also limits the total output energy. It is
        # not a claim that individual sample peaks cannot exceed full scale.
        self.assertLessEqual(np.linalg.norm(output), np.linalg.norm(samples) * 10 ** (6 / 20))
        rates = np.asarray(metadata["modulation_rates_hz"])
        self.assertTrue(((rates >= 0.14) & (rates <= 0.34)).all())

    def test_identical_seed_is_reproducible_and_different_seed_changes_audio(self):
        self.write(np.random.default_rng(5).uniform(-0.15, 0.15, (44100, 2)))
        first, _, profile = self.process(seed="a", filename="first.wav")
        second, _, repeated = self.process(seed="a", filename="second.wav")
        other, _, different = self.process(seed="b", filename="other.wav")
        np.testing.assert_array_equal(first, second)
        self.assertEqual(profile, repeated)
        self.assertNotEqual(profile["seed_sha256"], different["seed_sha256"])
        self.assertGreater(np.max(np.abs(first - other)), 0.01)

    def test_invalid_gain_source_format_and_nonfinite_samples_are_rejected(self):
        self.write(np.zeros((10, 2)))
        for gain in (float("nan"), float("inf"), -1, 6.01, True, "6"):
            with self.subTest(gain=gain), self.assertRaises(ValueError):
                self.process(gain=gain)
        self.write(np.zeros((10, 2)), rate=4000)
        with self.assertRaises(ValueError):
            self.process()
        self.write(np.empty((0, 2)))
        with self.assertRaises(ValueError):
            self.process()
        samples = np.zeros((600, 2))
        samples[-1, 0] = float("nan")
        self.write(samples)
        with self.assertRaisesRegex(ValueError, "non-finite"):
            self.process()

    def test_cancellation_before_creation_and_during_processing(self):
        self.write(np.zeros((44100, 2)))
        with self.assertRaises(JobCancelled):
            self.process(context=_Context(cancel_after=1))
        self.assertFalse((self.root / "out.wav").exists())
        with self.assertRaises(JobCancelled):
            self.process(context=_Context(cancel_after=12))
        self.assertLess(sf.info(self.root / "out.wav").frames, 44100)

    def test_source_cannot_be_overwritten(self):
        self.write(np.ones((10, 1)) * 0.1)
        with self.assertRaisesRegex(ValueError, "overwrite"):
            dynamic_instrumental_eq(_Context(), self.source, self.source, seed="a")


class SharedSmoothRemapAudioTests(unittest.TestCase):
    def test_shared_mapping_keeps_stereo_relation_and_stays_in_pitch_and_peak_bounds(self):
        from scipy.signal import hilbert

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            times = np.arange(3 * 44100 + 71) / 44100
            mono = 0.95 * np.sin(2 * np.pi * 440 * times)
            sf.write(root / "input.wav", np.column_stack((mono, -0.5 * mono)), 44100, subtype="DOUBLE")
            metadata = shared_smooth_remap(_Context(), root / "input.wav", root / "output.wav")
            output, rate = sf.read(root / "output.wav")
            self.assertEqual(rate, 44100)
            self.assertLess(len(output), len(times))
            self.assertLess(len(times) / len(output), 1.02)
            np.testing.assert_allclose(output[:, 1], -0.5 * output[:, 0], atol=1e-14, rtol=0)
            # Independent frequency measurement of a known tone, rather than
            # trusting the implementation's low-frequency audit markers.
            phase = np.unwrap(np.angle(hilbert(output[:, 0])))
            frequency = np.diff(phase) * rate / (2 * np.pi)
            self.assertGreater(np.median(frequency[4410:-4410]), 445)
            self.assertLess(np.median(frequency[4410:-4410]), 450)
            self.assertGreater(metadata["measured_pitch_min_cents"], 19)
            self.assertLess(metadata["measured_pitch_max_cents"], 37)
            self.assertLessEqual(estimate_audio_true_peak(_Context(), root / "output.wav")["peak_dbtp"], -1.09)
            self.assertLess(metadata["shared_gain"], 1)
            again = root / "again.wav"
            repeated = shared_smooth_remap(_Context(), root / "input.wav", again)
            np.testing.assert_array_equal(output, sf.read(again)[0])
            self.assertEqual(metadata, repeated)

    def test_peak_estimate_keeps_block_edges_and_detects_intersample_peak(self):
        from scipy.signal import resample_poly

        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "peaks.wav"
            times = np.arange(65_536 + 71) / 44100
            signal = 0.95 * np.sin(2 * np.pi * 11025 * times + np.pi / 4)
            signal[65_535:65_538] = [0.7, -0.8, 0.9]
            samples = np.column_stack((signal, -signal))
            sf.write(path, samples, 44100, subtype="DOUBLE")
            expected = float(np.max(np.abs(resample_poly(samples, 8, 1, axis=0))))
            measured = estimate_audio_true_peak(_Context(), path)
            self.assertAlmostEqual(measured["peak_linear"], expected, places=12)
            self.assertGreater(measured["peak_linear"], np.max(np.abs(samples)))
            self.assertEqual(measured["method"], "scipy_resample_poly_8x_kaiser5")


if __name__ == "__main__":
    unittest.main()
