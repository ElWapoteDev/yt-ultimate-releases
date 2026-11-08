"""Bounded instrumental EQ and gentle shared remapping, without recognition guarantees."""

from __future__ import annotations

import hashlib
import math
import tempfile
from pathlib import Path
from typing import Any

from ytultimate.jobs import JobContext


_FFT_SIZE = 2048
_HOP_SIZE = 512
_BAND_CENTERS = (100, 220, 500, 1100, 2500, 5500, 11000)
_BAND_WIDTH_OCTAVES = 0.55

_REMAP_BASE_SEMITONES = 0.28
_REMAP_AMPLITUDES = (0.05, 0.03)
_REMAP_PERIODS = (13.0, 23.0)
_REMAP_PHASES = (0.0, 1.1)
_REMAP_PADDING = 1024
_REMAP_BLOCK = 441
_TRUE_PEAK_METHOD = "scipy_resample_poly_8x_kaiser5"


def estimate_audio_true_peak(context: JobContext, source: Path) -> dict[str, Any]:
    """Estimate intersample peaks with bounded, overlapping 8x FIR blocks.

    This is a named engineering estimator, not an IEC true-peak certification.
    The 64-sample margins exceed the resampler FIR's support and are discarded.
    """
    import numpy as np
    import soundfile as sf
    from scipy.signal import resample_poly

    context.raise_if_cancelled()
    peak = 0.0
    block_size, margin = 65_536, 64
    with sf.SoundFile(source) as audio:
        if audio.frames <= 0:
            raise ValueError("Cannot measure an empty audio file")
        for position in range(0, audio.frames, block_size):
            context.raise_if_cancelled()
            start = max(0, position - margin)
            stop = min(audio.frames, position + block_size + margin)
            audio.seek(start)
            block = audio.read(stop - start, dtype="float64", always_2d=True)
            if len(block) != stop - start or not np.isfinite(block).all():
                raise ValueError("Audio contains incomplete or non-finite samples")
            upsampled = resample_poly(block, 8, 1, axis=0)
            begin = (position - start) * 8
            end = (min(audio.frames, position + block_size) - start) * 8
            peak = max(peak, float(np.max(np.abs(upsampled[begin:end]))))
    context.raise_if_cancelled()
    return {
        "method": _TRUE_PEAK_METHOD, "oversampling": 8,
        "peak_linear": peak, "peak_dbtp": 20 * math.log10(peak) if peak else None,
    }


def shared_smooth_remap(
    context: JobContext, source: Path, destination: Path,
) -> dict[str, Any]:
    """Apply the tested gentle varispeed to the complete stereo mix.

    One SoXR instance processes both channels and two low-frequency audit
    markers. Vocal and instrumental contributions therefore share the same
    remapping. A disk intermediate allows one constant headroom gain without
    retaining the song in memory. All files owned here are cleaned on failure.
    """
    import numpy as np
    import soundfile as sf
    import soxr

    source, destination = Path(source), Path(destination)
    if (source.resolve() == destination.resolve()
            or (destination.exists() and source.samefile(destination))):
        raise ValueError("Shared remap must not overwrite its source")
    context.raise_if_cancelled()
    with sf.SoundFile(source) as audio:
        frames, rate, channels = audio.frames, audio.samplerate, audio.channels
        if rate != 44_100 or channels != 2 or frames < 2:
            raise ValueError("Shared remap requires nonempty 44.1 kHz stereo audio")
    if soxr.__version__ != "1.1.0":
        raise ValueError("CALIDAD shared remap requires the tested SoXR 1.1.0 runtime")

    duration = (frames - 1) / rate
    initial_ratio = 2 ** (_REMAP_BASE_SEMITONES / 12)
    maximum_ratio = 2 ** ((_REMAP_BASE_SEMITONES + sum(_REMAP_AMPLITUDES)) / 12)
    output_created = False
    try:
        with tempfile.TemporaryDirectory(prefix="quality-remap-", dir=destination.parent) as temporary:
            raw = Path(temporary) / "unscaled.wav"
            stream = soxr.ResampleStream(
                rate * maximum_ratio, rate, channels + 2,
                dtype="float64", quality="HQ", vr=True,
            )
            # A nonzero initial slew leaves this experimental API at the
            # constructor's maximum ratio. Initialize immediately, then slew.
            stream.set_io_ratio(rate * initial_ratio, rate, slew_len=0)
            previous_ratio = initial_ratio
            last_phase = None
            last_position = None
            first_position = None
            phase_tail = np.empty(0, dtype=np.float64)
            slope_window = round(rate * 0.05)
            minimum_cents, maximum_cents = math.inf, -math.inf
            minimum_step, maximum_step = math.inf, -math.inf
            marker_error = 0.0
            written = raw_frames = prefix = suffix = 0
            finished = False
            total = frames + 2 * _REMAP_PADDING
            with (sf.SoundFile(source) as audio,
                  sf.SoundFile(raw, "w", samplerate=rate, channels=channels,
                               format="WAV", subtype="DOUBLE") as output):
                for position in range(0, total, _REMAP_BLOCK):
                    context.raise_if_cancelled()
                    count = min(_REMAP_BLOCK, total - position)
                    indexes = np.arange(position, position + count) - _REMAP_PADDING
                    block = np.zeros((count, channels + 2), dtype=np.float64)
                    valid_input = (indexes >= 0) & (indexes < frames)
                    expected = int(np.count_nonzero(valid_input))
                    if expected:
                        samples = audio.read(expected, dtype="float64", always_2d=True)
                        if len(samples) != expected or not np.isfinite(samples).all():
                            raise ValueError("Shared remap source contains incomplete or non-finite audio")
                        block[valid_input, :channels] = samples
                    marker_phase = 2 * np.pi * indexes / rate
                    block[:, channels] = np.sin(marker_phase)
                    block[:, channels + 1] = np.cos(marker_phase)
                    time_seconds = (position + count - _REMAP_PADDING) / rate
                    taper_in = math.sin(math.pi / 2 * min(1, max(0, time_seconds / 3))) ** 2
                    taper_out = math.sin(math.pi / 2 * min(1, max(0, (duration - time_seconds) / 3))) ** 2
                    modulation = sum(
                        amplitude * math.sin(2 * math.pi * time_seconds / period + phase)
                        for amplitude, period, phase in zip(_REMAP_AMPLITUDES, _REMAP_PERIODS, _REMAP_PHASES)
                    )
                    ratio = 2 ** ((_REMAP_BASE_SEMITONES + taper_in * taper_out * modulation) / 12)
                    # Upstream C defines slew_len in OUTPUT samples; the
                    # Python docstring in 1.1.0 incorrectly calls them input.
                    stream.set_io_ratio(rate * ratio, rate, slew_len=max(1, round(count / previous_ratio)))
                    context.raise_if_cancelled()
                    result = stream.resample_chunk(block, last=position + count == total)
                    previous_ratio = ratio
                    if not len(result):
                        continue
                    if not np.isfinite(result).all():
                        raise ValueError("Shared remap produced non-finite audio")
                    raw_frames += len(result)
                    phases = np.arctan2(result[:, channels], result[:, channels + 1])
                    if last_phase is None:
                        phases = np.unwrap(phases)
                    else:
                        phases = np.unwrap(np.concatenate(([last_phase], phases)))[1:]
                    last_phase = float(phases[-1])
                    mapping = phases * rate / (2 * math.pi)
                    amplitude = np.hypot(result[:, channels], result[:, channels + 1])
                    valid = (amplitude > 0.99) & (mapping >= 0) & (mapping <= frames - 1)
                    selected = np.flatnonzero(valid)
                    if not len(selected):
                        if written:
                            suffix += len(result)
                            finished = True
                        else:
                            prefix += len(result)
                        continue
                    begin, end = int(selected[0]), int(selected[-1]) + 1
                    if finished or not valid[begin:end].all() or (written and begin):
                        raise ValueError("Shared remap produced a discontinuous time map")
                    if not written:
                        prefix += begin
                    suffix += len(result) - end
                    finished = end < len(result)
                    mapping = mapping[begin:end]
                    steps = np.diff(mapping if last_position is None else np.concatenate(([last_position], mapping)))
                    if len(steps):
                        if np.min(steps) <= 0:
                            raise ValueError("Shared remap reversed its time map")
                        minimum_step = min(minimum_step, float(np.min(steps)))
                        maximum_step = max(maximum_step, float(np.max(steps)))
                    first_position = float(mapping[0]) if first_position is None else first_position
                    last_position = float(mapping[-1])
                    joined = np.concatenate((phase_tail, mapping))
                    if len(joined) > slope_window:
                        slopes = (joined[slope_window:] - joined[:-slope_window]) / slope_window
                        cents = 1200 * np.log2(slopes)
                        minimum_cents = min(minimum_cents, float(np.min(cents)))
                        maximum_cents = max(maximum_cents, float(np.max(cents)))
                        if np.max(np.abs(cents)) > 50:
                            raise ValueError("Shared remap exceeded its half-semitone limit")
                    phase_tail = joined[-slope_window:]
                    marker_error = max(marker_error, float(np.max(np.abs(amplitude[begin:end] - 1))))
                    context.raise_if_cancelled()
                    output.write(result[begin:end, :channels])
                    written += end - begin
            if (not written or first_position is None or last_position is None
                    or first_position > 2 or frames - 1 - last_position > 2
                    or frames > written * 1.02 + 2):
                raise ValueError("Shared remap did not preserve the complete source interval")
            context.raise_if_cancelled()
            peak = estimate_audio_true_peak(context, raw)
            gain = min(1.0, 10 ** (-1.1 / 20) / peak["peak_linear"]) if peak["peak_linear"] else 1.0
            context.raise_if_cancelled()
            with (sf.SoundFile(raw) as audio,
                  sf.SoundFile(destination, "w", samplerate=rate, channels=channels,
                               format="WAV", subtype="DOUBLE") as output):
                output_created = True
                while True:
                    context.raise_if_cancelled()
                    samples = audio.read(65_536, dtype="float64", always_2d=True)
                    if not len(samples):
                        break
                    context.raise_if_cancelled()
                    output.write(samples * gain)
            context.raise_if_cancelled()
            return {
                "method": "shared_smooth_remap", "version": 1, "soxr_version": soxr.__version__,
                "quality": "HQ", "base_pitch_semitones": _REMAP_BASE_SEMITONES,
                "modulation_amplitudes_semitones": list(_REMAP_AMPLITUDES),
                "modulation_periods_seconds": list(_REMAP_PERIODS),
                "modulation_phases_radians": list(_REMAP_PHASES), "edge_taper_seconds": 3.0,
                "input_frames": frames, "output_frames": written, "sample_rate": rate, "channels": channels,
                "mean_speed_ratio": frames / written, "shared_gain": gain,
                "shared_gain_db": 20 * math.log10(gain),
                "true_peak_method": _TRUE_PEAK_METHOD, "true_peak_target_dbtp": -1.1,
                "max_allowed_true_peak_dbtp": -1.0, "pre_gain_true_peak_dbtp": peak["peak_dbtp"],
                "map_method": "quadrature_1hz_estimate", "map_first_source_sample": first_position,
                "map_last_source_sample": last_position, "map_min_increment": minimum_step if math.isfinite(minimum_step) else None,
                "map_max_increment": maximum_step if math.isfinite(maximum_step) else None,
                "measured_pitch_min_cents": minimum_cents if math.isfinite(minimum_cents) else None,
                "measured_pitch_max_cents": maximum_cents if math.isfinite(maximum_cents) else None,
                "measured_pitch_window_frames": slope_window, "marker_amplitude_max_error": marker_error,
                "padding_input_frames_each_end": _REMAP_PADDING, "input_block_frames": _REMAP_BLOCK,
                "initial_ratio": initial_ratio, "constructor_max_ratio": maximum_ratio, "initial_slew": 0,
                "slew_convention": "output_samples", "flush_calls": 1,
                "raw_output_frames_with_padding": raw_frames,
                "removed_output_prefix": prefix, "removed_output_suffix": suffix,
                "reported_delay_after_flush": float(stream.delay()),
            }
    except BaseException:
        if output_created:
            destination.unlink(missing_ok=True)
        raise


def dynamic_instrumental_eq(
    context: JobContext,
    source: Path,
    destination: Path,
    *,
    seed: str | int,
    max_gain_db: float = 6.0,
) -> dict[str, Any]:
    """Apply deterministic, smoothly varying spectral EQ with bounded memory.

    Only the supplied instrumental is read. A shared stereo spectral envelope
    retains channel balance and introduces no resampling, pitch shift, delay,
    or change to the number of samples. The caller owns temporary-file cleanup.
    """
    import numpy as np
    import soundfile as sf

    if (isinstance(max_gain_db, bool) or not isinstance(max_gain_db, (int, float))
            or not math.isfinite(max_gain_db) or not 0 <= max_gain_db <= 6):
        raise ValueError("Instrumental EQ gain must be finite and between 0 and 6 dB")
    source, destination = Path(source), Path(destination)
    if (source.resolve() == destination.resolve()
            or (destination.exists() and source.samefile(destination))):
        raise ValueError("Instrumental EQ must not overwrite its source")
    context.raise_if_cancelled()
    digest = hashlib.sha256(str(seed).encode("utf-8")).digest()

    with sf.SoundFile(source) as audio:
        rate, channels, frames = audio.samplerate, audio.channels, audio.frames
        if not 8_000 <= rate <= 384_000 or not 1 <= channels <= 8 or frames <= 0:
            raise ValueError("Invalid instrumental sample rate, channels, or duration")
        centers = [value for value in _BAND_CENTERS if value < rate / 2]
        phases = [
            2 * math.pi * int.from_bytes(digest[index * 2:index * 2 + 2], "little") / 65535
            for index in range(len(centers))
        ]
        modulation_rates = [
            0.14 + 0.20 * int.from_bytes(digest[14 + index * 2:16 + index * 2], "little") / 65535
            for index in range(len(centers))
        ]
        frequencies = np.fft.rfftfreq(_FFT_SIZE, d=1 / rate)
        log_frequencies = np.log2(np.maximum(frequencies, 20))
        band_weights = np.exp(-0.5 * (
            (log_frequencies[:, None] - np.log2(centers)[None, :]) / _BAND_WIDTH_OCTAVES
        ) ** 2)
        phases = np.asarray(phases)
        modulation_rates = np.asarray(modulation_rates)
        # Square-root periodic Hann on both sides, with explicit overlap
        # normalization, reconstructs arbitrary lengths and both edge samples.
        window = np.sqrt(np.hanning(_FFT_SIZE + 1)[:-1])
        window_squared = window * window
        input_frame = np.zeros((_FFT_SIZE, channels), dtype=np.float64)
        overlap = np.zeros_like(input_frame)
        overlap_weight = np.zeros(_FFT_SIZE, dtype=np.float64)
        position = -_FFT_SIZE + _HOP_SIZE
        written = 0
        with sf.SoundFile(
            destination, "w", samplerate=rate, channels=channels,
            format="WAV", subtype="DOUBLE",
        ) as output:
            while position < frames:
                context.raise_if_cancelled()
                block = audio.read(_HOP_SIZE, dtype="float64", always_2d=True)
                if not np.isfinite(block).all():
                    raise ValueError("Instrumental contains non-finite audio samples")
                input_frame[:-_HOP_SIZE] = input_frame[_HOP_SIZE:]
                input_frame[-_HOP_SIZE:] = 0
                begin = _FFT_SIZE - _HOP_SIZE
                input_frame[begin:begin + len(block)] = block
                spectrum = np.fft.rfft(input_frame * window[:, None], axis=0)
                center_time = (position + _FFT_SIZE / 2) / rate
                gains_db = band_weights @ (
                    max_gain_db * np.sin(2 * math.pi * modulation_rates * center_time + phases)
                )
                np.clip(gains_db, -max_gain_db, max_gain_db, out=gains_db)
                spectrum *= (10 ** (gains_db / 20))[:, None]
                processed = np.fft.irfft(spectrum, n=_FFT_SIZE, axis=0) * window[:, None]
                if not np.isfinite(processed).all():
                    raise ValueError("Instrumental EQ produced non-finite audio samples")
                overlap += processed
                overlap_weight += window_squared
                if position >= 0:
                    count = min(_HOP_SIZE, frames - position)
                    context.raise_if_cancelled()
                    output.write(overlap[:count] / overlap_weight[:count, None])
                    written += count
                overlap[:-_HOP_SIZE] = overlap[_HOP_SIZE:]
                overlap[-_HOP_SIZE:] = 0
                overlap_weight[:-_HOP_SIZE] = overlap_weight[_HOP_SIZE:]
                overlap_weight[-_HOP_SIZE:] = 0
                position += _HOP_SIZE
            context.raise_if_cancelled()
            if written != frames:
                raise ValueError("Instrumental EQ did not preserve the sample count")

    return {
        "method": "dynamic_spectral_eq",
        "version": 1,
        "seed_sha256": digest.hex(),
        "max_gain_db": float(max_gain_db),
        "sample_rate": rate,
        "channels": channels,
        "frames": frames,
        "fft_size": _FFT_SIZE,
        "hop_size": _HOP_SIZE,
        "window": "sqrt_periodic_hann",
        "band_centers_hz": centers,
        "band_width_octaves": _BAND_WIDTH_OCTAVES,
        "modulation_rates_hz": modulation_rates.tolist(),
        "phases_radians": phases.tolist(),
    }
