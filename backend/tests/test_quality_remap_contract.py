from __future__ import annotations

import hashlib
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import numpy as np
import soundfile as sf

from ytultimate.jobs import JobCancelled
from ytultimate.services.quality_audio import shared_smooth_remap


class _Context:
    def __init__(self, cancel_after=None):
        self.checks = 0
        self.cancel_after = cancel_after

    def raise_if_cancelled(self):
        self.checks += 1
        if self.cancel_after is not None and self.checks >= self.cancel_after:
            raise JobCancelled("cancelled by contract test")


class SharedRemapContractTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "input.wav"
        self.destination = self.root / "output.wav"

    def write(self, samples, rate=44100):
        sf.write(self.source, samples, rate, subtype="DOUBLE")

    def source_digest(self):
        return hashlib.sha256(self.source.read_bytes()).hexdigest()

    def assert_only_source_remains(self):
        self.assertEqual(
            {path.relative_to(self.root) for path in self.root.rglob("*") if path.is_file()},
            {Path("input.wav")},
        )

    def test_source_and_hardlink_alias_cannot_be_overwritten(self):
        self.write(np.full((4410, 2), 0.1))
        digest = self.source_digest()
        alias = self.root / "alias.wav"
        alias.hardlink_to(self.source)
        for destination in (self.source, alias):
            with self.subTest(destination=destination.name), self.assertRaises(ValueError):
                shared_smooth_remap(_Context(), self.source, destination)
            self.assertTrue(self.source.is_file())
            self.assertTrue(alias.is_file())
            self.assertEqual(self.source_digest(), digest)

    def test_invalid_shapes_and_nonfinite_audio_fail_without_destroying_source(self):
        cases = [
            (np.zeros((0, 2)), 44100),
            (np.zeros((1, 2)), 44100),
            (np.zeros((4410, 1)), 44100),
            (np.zeros((4410, 2)), 48000),
        ]
        for invalid in (float("nan"), float("inf"), -float("inf")):
            samples = np.zeros((4410, 2))
            samples[-7, 1] = invalid
            cases.append((samples, 44100))
        for samples, rate in cases:
            with self.subTest(shape=samples.shape, rate=rate, invalid=not np.isfinite(samples).all()):
                self.write(samples, rate)
                digest = self.source_digest()
                with self.assertRaises(ValueError):
                    shared_smooth_remap(_Context(), self.source, self.destination)
                self.assertEqual(self.source_digest(), digest)
                self.assert_only_source_remains()

    def test_cancellation_before_creation_and_midstream_cleans_its_files(self):
        self.write(np.full((44100 * 3, 2), 0.05))
        digest = self.source_digest()
        for check in (1, 30):
            with self.subTest(cancel_after=check), self.assertRaises(JobCancelled):
                shared_smooth_remap(_Context(check), self.source, self.destination)
            self.assertEqual(self.source_digest(), digest)
            self.assert_only_source_remains()

    def test_first_and_last_transients_survive_flush_for_odd_length_audio(self):
        frames = 44100 + 137
        samples = np.zeros((frames, 2))
        pulse = np.hanning(201) * 0.2
        samples[:201, 0] = pulse
        samples[-201:, 1] = pulse
        self.write(samples)
        digest = self.source_digest()
        metadata = shared_smooth_remap(_Context(), self.source, self.destination)
        output, rate = sf.read(self.destination, dtype="float64", always_2d=True)
        self.assertEqual(rate, 44100)
        self.assertEqual(output.shape[1], 2)
        self.assertEqual(sf.info(self.destination).subtype, "DOUBLE")
        self.assertGreater(np.max(np.abs(output[:500, 0])), 0.1)
        self.assertGreater(np.max(np.abs(output[-500:, 1])), 0.1)
        self.assertLess(np.max(np.abs(output[:500, 1])), 1e-8)
        self.assertLess(np.max(np.abs(output[-500:, 0])), 1e-8)
        self.assertGreaterEqual(len(output), frames / 1.02 - 2)
        self.assertLessEqual(len(output), frames)
        self.assertEqual(metadata["input_frames"], frames)
        self.assertEqual(metadata["output_frames"], len(output))
        self.assertEqual(self.source_digest(), digest)

    def test_longer_input_is_read_and_combined_in_bounded_blocks(self):
        # A 32-second source is larger than any allowed working block. The
        # guards catch whole-file reads and accumulation followed by concat.
        with sf.SoundFile(self.source, "w", samplerate=44100, channels=2,
                          format="WAV", subtype="DOUBLE") as source:
            block = np.zeros((44100, 2))
            for _ in range(32):
                source.write(block)
        read = sf.SoundFile.read
        concatenate = np.concatenate
        reads = []

        def bounded_read(handle, frames=-1, *args, **kwargs):
            self.assertGreaterEqual(frames, 0, "whole-file audio read")
            self.assertLessEqual(frames, 262144, "unbounded audio read")
            reads.append(frames)
            return read(handle, frames, *args, **kwargs)

        def bounded_concatenate(arrays, *args, **kwargs):
            arrays = tuple(arrays)
            axis = kwargs.get("axis", args[0] if args else 0)
            if axis == 0 and arrays and all(getattr(a, "ndim", 0) == 2 for a in arrays):
                self.assertLessEqual(sum(len(a) for a in arrays), 262144,
                                     "accumulated audio blocks")
            return concatenate(arrays, *args, **kwargs)

        with (mock.patch.object(sf.SoundFile, "read", bounded_read),
              mock.patch.object(np, "concatenate", bounded_concatenate)):
            metadata = shared_smooth_remap(_Context(), self.source, self.destination)
        self.assertGreater(len(reads), 32)
        self.assertEqual(metadata["input_frames"], 32 * 44100)
        self.assertGreater(metadata["output_frames"], 31 * 44100)


if __name__ == "__main__":
    unittest.main()
