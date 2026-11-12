"""Real CALIDAD inference using the desktop's environment, never a test override."""

from __future__ import annotations

import hashlib
import json
import os
import sys
import time
from pathlib import Path

import numpy as np
import soundfile as sf

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from ytultimate.config import AppConfig
from ytultimate.jobs import JobManager, JobState
from ytultimate.services.media import MediaTools
from ytultimate.services.processing import ProcessingService


def synthetic_source(path: Path) -> None:
    rate = 48_000
    times = np.arange(rate * 14) / rate
    notes = np.array([220, 261.63, 329.63, 293.66, 392, 349.23, 246.94, 440])
    melody = notes[(times * 2).astype(int) % len(notes)]
    phase = np.cumsum(2 * np.pi * melody / rate)
    vocal = sum(
        np.sin(phase * harmonic + 0.04 * np.sin(2 * np.pi * 5 * times)) / harmonic
        for harmonic in range(1, 7)
    )
    vocal *= 0.14 * (0.4 + 0.6 * np.sin(np.pi * (times % 0.5) / 0.5) ** 2)
    backing = 0.16 * np.sin(2 * np.pi * 110 * times) + 0.11 * np.sin(phase * 1.5)
    percussion = (0.07 * np.random.default_rng(7).normal(size=len(times))
                  * np.exp(-20 * (times % 0.5)))
    sf.write(path, np.column_stack([
        vocal + backing + percussion, vocal + 0.9 * backing - percussion,
    ]), rate, subtype="PCM_24")


def main() -> None:
    temporary = Path(sys.argv[1]).resolve()
    assert sys.flags.optimize == 0, "Ambient optimization can hide a broken separator launch"
    # The same factory the desktop backend uses must preserve the venv Python
    # path and optimized separator prefix supplied by BackendController.
    config = AppConfig.from_env()
    for isolated in (config.data_dir, config.cache_dir, config.log_dir,
                     config.library_dir, config.plugin_dir, config.database_path):
        assert isolated.is_relative_to(temporary), f"Non-isolated smoke path: {isolated}"
    assert config.separator_path == os.environ["YTU_SEPARATOR_PATH"]
    assert config.separator_args == tuple(json.loads(os.environ["YTU_SEPARATOR_ARGS"]))
    config.ensure_directories()
    source = config.library_dir / "Synthetic-quality.wav"
    synthetic_source(source)
    original = hashlib.sha256(source.read_bytes()).hexdigest()
    phases = set()

    def report_phase(_name, payload):
        phase = payload.get("metadata", {}).get("phase")
        if phase and phase not in phases:
            phases.add(phase)
            print(f"[dev-processing] {phase}", flush=True)

    started = time.monotonic()
    timeout = int(os.environ["YTU_DEV_PROCESSING_TIMEOUT_MS"]) / 1000
    with JobManager(max_workers=1) as jobs:
        service = ProcessingService(config, jobs, MediaTools(config), events=report_phase)
        submitted = service.start_fingerprint(source, intensity="quality")
        while True:
            current = jobs.get(submitted.id)
            if current.status in (JobState.COMPLETED, JobState.FAILED, JobState.CANCELLED):
                break
            if time.monotonic() - started > timeout:
                jobs.cancel(submitted.id)
                raise AssertionError("CALIDAD exceeded the real-inference timeout")
            time.sleep(0.5)
        assert current.status == JobState.COMPLETED, current.error
        output = current.result
    assert hashlib.sha256(source.read_bytes()).hexdigest() == original
    info = sf.info(output["local_path"])
    assert info.format == "FLAC" and info.subtype == "PCM_24", info
    assert info.samplerate == 44_100 and info.channels == 2, info
    profile = output["fingerprint_profile"]
    assert profile["vocals_preserved"] is False
    assert profile["vocal_processing"] == "shared_smooth_remap"
    assert profile["processing_target"] == "instrumental_then_full_mix"
    assert profile["pitch_semitones"] == 0.28
    assert profile["pitch_range_semitones"] == [0.20, 0.36]
    assert profile["source_sample_rate"] == 48_000
    assert profile["version"] == 6
    assert profile["instrumental_processing"]["method"] == "dynamic_spectral_eq"
    assert profile["instrumental_processing"]["max_gain_db"] == 6
    pre_audit = profile["pre_remap_vocal_integrity"]
    assert pre_audit["status"] == "verified"
    assert pre_audit["scope"] == "separated_stem_before_shared_remap"
    assert pre_audit["frames"] == 14 * 44_100
    assert pre_audit["max_abs_error"] <= pre_audit["tolerance"] == 2 ** -23
    remap = profile["shared_processing"]
    assert remap["input_frames"] == 14 * 44_100
    assert remap["output_frames"] == info.frames
    assert 1 < remap["mean_speed_ratio"] <= 1.02
    assert profile["speed"] == remap["mean_speed_ratio"]
    assert 0 <= remap["measured_pitch_min_cents"] <= remap["measured_pitch_max_cents"] <= 50
    assert 0 < remap["shared_gain"] <= 1
    audit = profile["final_audio_integrity"]
    assert audit["status"] == "verified" and audit["scope"] == "processed_full_mix"
    assert audit["frames"] == info.frames and audit["channels"] == info.channels
    assert audit["max_abs_error"] <= audit["tolerance"] == 2 ** -23
    assert audit["true_peak_estimate_dbtp"] <= -1
    verification = output["fingerprint_verification"]
    assert verification["pcm_changed"]
    assert verification["status"] in {"changed", "unchanged"}, verification
    assert min(verification["source_frames"], verification["output_frames"]) >= 16
    assert verification["local_comparison"]["metric"] == "chromaprint_bit_agreement_v1"
    assert verification["recognition_tested"] is False
    assert not list(config.stems_dir.glob("**/*")), "Intermediate stems were published"
    assert not list((config.cache_dir / "Processing" / "Quality").glob("*")), \
        "Temporary processing files remain"
    assert {"separating", "processing", "recombining", "remapping", "verifying"}.issubset(phases), phases
    print(json.dumps({
        "elapsed_seconds": round(time.monotonic() - started, 1),
        "duration": info.duration,
        "sample_rate": info.samplerate,
        "subtype": info.subtype,
        "shared_gain": remap["shared_gain"],
        "pre_remap_vocal_max_abs_error": pre_audit["max_abs_error"],
        "final_mix_max_abs_error": audit["max_abs_error"],
        "final_true_peak_estimate_dbtp": audit["true_peak_estimate_dbtp"],
        "mean_speed_ratio": remap["mean_speed_ratio"],
        "local_bit_agreement": verification["local_comparison"]["best_offset"]["bit_similarity"],
        "verification": verification["status"],
    }), flush=True)


if __name__ == "__main__":
    main()
