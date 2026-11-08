"""Local audio verification; a changed fingerprint is not a failed recognition match."""

from __future__ import annotations

import hashlib
import json
import math
import re
from pathlib import Path
from typing import Any

from ytultimate.jobs import JobContext
from ytultimate.services.media import CommandExecutionError, MediaToolError, MediaTools


MIN_FINGERPRINT_FRAMES = 16
MAX_ALIGNMENT_SHIFT_FRAMES = 16
MIN_ALIGNMENT_OVERLAP = 0.8


def pcm_digest(media: MediaTools, path: Path, context: JobContext) -> str:
    # Hash decoded samples, never container tags, artwork or the generated ID.
    result = media.run_ffmpeg(
        ["-v", "error", "-nostdin", "-i", path, "-map", "0:a:0",
         "-ac", "2", "-ar", "44100", "-c:a", "pcm_s16le",
         "-f", "hash", "-hash", "sha256", "-"],
        context=context,
    )
    match = re.fullmatch(r"SHA256=([0-9a-fA-F]{64})", result.stdout.strip())
    if not match:
        raise MediaToolError("No se pudo verificar el audio decodificado")
    return match[1].lower()


def acoustic_fingerprint(media: MediaTools, path: Path, context: JobContext) -> list[int]:
    try:
        result = media.run_executable(
            media.config.fpcalc_path,
            ["-algorithm", "2", "-length", "120", "-raw", "-json", path],
            context=context,
            timeout=180,
        )
    except CommandExecutionError as exc:
        if exc.returncode == 2 and exc.stderr.strip() == "ERROR: Empty fingerprint":
            return []
        raise
    try:
        values = json.loads(result.stdout)["fingerprint"]
        if not isinstance(values, list) or not all(
            type(value) is int and 0 <= value <= 0xFFFFFFFF for value in values
        ):
            raise ValueError("Invalid fingerprint frames")
        return values
    except (ValueError, TypeError, KeyError) as exc:
        raise MediaToolError("Chromaprint devolvió una huella inválida") from exc


def compare_fingerprint_frames(source: list[int], output: list[int]) -> dict[str, Any]:
    """Describe bit agreement, not recognition confidence or an AcoustID match.

    A small global offset search keeps a short leading delay from looking like
    a large spectral change. It cannot align tempo changes, compare recordings
    against an index, or represent a proprietary detector's decision.
    """
    shortest = min(len(source), len(output))
    if shortest < MIN_FINGERPRINT_FRAMES:
        raise ValueError("Not enough fingerprint frames for a local comparison")
    minimum_overlap = max(MIN_FINGERPRINT_FRAMES, math.ceil(shortest * MIN_ALIGNMENT_OVERLAP))

    def at_offset(offset: int) -> dict[str, Any]:
        # Positive means the corresponding frame occurs later in the output.
        source_start = max(0, -offset)
        output_start = max(0, offset)
        count = min(len(source) - source_start, len(output) - output_start)
        differences = [
            source[source_start + index] ^ output[output_start + index]
            for index in range(count)
        ]
        return {
            "offset_frames": offset,
            "compared_frames": count,
            "bit_similarity": 1 - sum(value.bit_count() for value in differences) / (32 * count),
            "exact_frame_fraction": sum(value == 0 for value in differences) / count,
        }

    aligned = at_offset(0)
    best = aligned

    def ranking(value: dict[str, Any]) -> tuple[float, int, int, int]:
        return (value["bit_similarity"], value["compared_frames"],
                -abs(value["offset_frames"]), -value["offset_frames"])

    for offset in range(-MAX_ALIGNMENT_SHIFT_FRAMES, MAX_ALIGNMENT_SHIFT_FRAMES + 1):
        if offset == 0:
            continue
        overlap = min(len(source) - max(0, -offset), len(output) - max(0, offset))
        if overlap < minimum_overlap:
            continue
        candidate = at_offset(offset)
        # Prefer more evidence and then less displacement on exact ties.
        if ranking(candidate) > ranking(best):
            best = candidate
    return {
        "metric": "chromaprint_bit_agreement_v1",
        "bits_per_frame": 32,
        "aligned": aligned,
        "best_offset": best,
        "max_offset_frames": MAX_ALIGNMENT_SHIFT_FRAMES,
        "min_overlap_fraction": MIN_ALIGNMENT_OVERLAP,
        "min_compared_frames": minimum_overlap,
        "offset_convention": "output_index_minus_source_index",
    }


def verify_audio_change(
    media: MediaTools, source: Path, output: Path, context: JobContext,
) -> dict[str, Any]:
    original = pcm_digest(media, source, context)
    modified = pcm_digest(media, output, context)
    if original == modified:
        raise MediaToolError("El procesamiento no cambió las muestras de audio")
    result: dict[str, Any] = {
        "pcm_changed": True,
        "source_pcm_sha256": original,
        "output_pcm_sha256": modified,
        "algorithm": "chromaprint-2",
        "max_analysis_seconds": 120,
        "recognition_tested": False,
    }
    if not media.config.fpcalc_path:
        return {**result, "status": "unavailable"}
    # Tool failures propagate: an installed but broken verifier must not silently
    # become a successful verification. Cancellation also propagates unchanged.
    before = acoustic_fingerprint(media, source, context)
    after = acoustic_fingerprint(media, output, context)
    result.update(source_frames=len(before), output_frames=len(after))
    if min(len(before), len(after)) < MIN_FINGERPRINT_FRAMES:
        return {**result, "status": "insufficient_audio"}
    for name, frames in (("source", before), ("output", after)):
        packed = b"".join(value.to_bytes(4, "little") for value in frames)
        result[f"{name}_fingerprint_sha256"] = hashlib.sha256(packed).hexdigest()
    # Length alone is not evidence of different acoustic features.
    result["status"] = "changed" if any(a != b for a, b in zip(before, after)) else "unchanged"
    result["local_comparison"] = compare_fingerprint_frames(before, after)
    return result
