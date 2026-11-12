from __future__ import annotations

import hashlib
import os
import re
import shutil
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Mapping

from ytultimate.config import (
    AppConfig,
    DEFAULT_SEPARATOR_MODEL,
    DEFAULT_SEPARATOR_MODEL_LABEL,
    SUPPORTED_AUDIO_EXTENSIONS,
)
from ytultimate.jobs import (
    ConcurrencyLimit,
    ConcurrencyLimiter,
    JobContext,
    JobManager,
    JobSnapshot,
)
from ytultimate.services.media import (
    MediaReadyCallback,
    MediaRegistrar,
    MediaTools,
    atomic_publish,
    dispatch_event,
    managed_content_path,
    register_media,
    reserve_unique_path,
    safe_filename,
)
from ytultimate.services.fingerprint import verify_audio_change


EventCallback = Callable[[str, Mapping[str, Any]], None]
_ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")
_PERCENT = re.compile(r"(\d{1,3}(?:\.\d+)?)%")
# The separator names its outputs after the input file. Windows keeps the
# 260-character path limit unless long paths are enabled system-wide, so long
# titles are separated from a short copy outside the scanned output directory.
_SHORT_SEPARATOR_INPUT = os.name == "nt"


class ProcessingError(RuntimeError):
    """Base error for local audio processing."""


@dataclass(frozen=True, slots=True)
class StemOutput:
    label: str
    stem: str
    filename: str
    local_path: str
    path: str
    track_id: str
    size_bytes: int

    def to_dict(self) -> dict[str, Any]:
        return {
            "label": self.label,
            "stem": self.stem,
            "filename": self.filename,
            "local_path": self.local_path,
            "path": self.path,
            "track_id": self.track_id,
            "size_bytes": self.size_bytes,
        }


class ProcessingService:
    """Job-backed local separation and fingerprint processing."""

    DEFAULT_FORMATS = {
        "FLAC": ".flac",
        "MP3": ".mp3",
        "WAV": ".wav",
    }

    def __init__(
        self,
        config: AppConfig,
        jobs: JobManager,
        media: MediaTools,
        *,
        registrar: MediaRegistrar | MediaReadyCallback | None = None,
        events: EventCallback | None = None,
        separator_formats: Mapping[str, str] | None = None,
        separator_model_label: str = DEFAULT_SEPARATOR_MODEL_LABEL,
        allow_external_sources: bool = False,
        concurrency_limit: ConcurrencyLimit | None = None,
    ) -> None:
        self.config = config
        self.jobs = jobs
        self.media = media
        self.registrar = registrar
        self.events = events
        self.separator_formats = {
            str(name).upper(): _normalize_extension(extension)
            for name, extension in (
                separator_formats or self.DEFAULT_FORMATS
            ).items()
        }
        if not self.separator_formats:
            raise ValueError("At least one separator output format is required")
        self.separator_model_label = separator_model_label
        self.allow_external_sources = allow_external_sources
        self._concurrency = ConcurrencyLimiter(concurrency_limit)

    def separator_info(self) -> dict[str, Any]:
        executable = self.config.separator_path
        executable_path = Path(executable) if executable else None
        executable_available = bool(
            executable
            and (
                (
                    executable_path is not None
                    and executable_path.is_file()
                    and os.access(executable_path, os.X_OK)
                )
                or shutil.which(executable)
            )
        )
        model_path = self.config.separator_model_path
        return {
            "available": executable_available,
            "model_ready": model_path.is_file(),
            "model": model_path.name or DEFAULT_SEPARATOR_MODEL,
            "model_label": self.separator_model_label,
            "model_dir": str(model_path.parent),
            "model_size_bytes": model_path.stat().st_size
            if model_path.is_file()
            else 0,
            "output_formats": list(self.separator_formats),
            "output_dir": str(self.config.stems_dir),
        }

    def start_stem_separation(
        self,
        source: Path | str,
        *,
        output_format: str = "FLAC",
        title: str | None = None,
        callback: EventCallback | None = None,
    ) -> JobSnapshot:
        source_path = self._resolve_source(source)
        normalized_format = output_format.upper()
        if normalized_format not in self.separator_formats:
            supported = ", ".join(self.separator_formats)
            raise ValueError(f"Unsupported separator format; choose {supported}")
        display_title = title or source_path.stem
        snapshot = self.jobs.submit(
            "stem_separation",
            self._separate_job,
            source_path,
            normalized_format,
            display_title,
            callback,
            metadata={
                "filename": source_path.name,
                "local_path": str(source_path),
                "title": display_title,
                "model": self.config.separator_model_path.name,
                "model_label": self.separator_model_label,
                "output_format": normalized_format,
                "phase": "pending",
                "outputs": [],
            },
        )
        self._emit("processing.stems.queued", snapshot.to_dict(), callback)
        return snapshot

    def start_fingerprint(
        self,
        source: Path | str,
        *,
        title: str | None = None,
        intensity: str = "strong",
        callback: EventCallback | None = None,
    ) -> JobSnapshot:
        if intensity not in {"balanced", "strong", "quality"}:
            raise ValueError("Fingerprint intensity must be balanced, strong or quality")
        source_path = self._resolve_source(source, audio_only=True)
        display_title = title or source_path.stem
        snapshot = self.jobs.submit(
            "fingerprint",
            self._fingerprint_job,
            source_path,
            display_title,
            callback,
            intensity,
            metadata={
                "filename": source_path.name,
                "local_path": str(source_path),
                "title": display_title,
                "phase": "pending",
                "output": None,
                "intensity": intensity,
            },
        )
        self._emit("processing.fingerprint.queued", snapshot.to_dict(), callback)
        return snapshot

    def start_chipmunk(
        self,
        source: Path | str,
        *,
        pitch_semitones: float = 7.0,
        speed: float = 1.0,
        brightness_db: float = 3.0,
        title: str | None = None,
        callback: EventCallback | None = None,
    ) -> JobSnapshot:
        source_path = self._resolve_source(source)
        display_title = title or source_path.stem
        filters, profile = self.chipmunk_profile(
            pitch_semitones,
            speed,
            brightness_db,
        )
        snapshot = self.jobs.submit(
            "chipmunk",
            self._chipmunk_job,
            source_path,
            display_title,
            filters,
            profile,
            callback,
            metadata={
                "filename": source_path.name,
                "local_path": str(source_path),
                "title": display_title,
                "phase": "pending",
                "profile": profile,
                "output": None,
            },
        )
        self._emit("processing.chipmunk.queued", snapshot.to_dict(), callback)
        return snapshot

    def get(self, job_id: str) -> JobSnapshot:
        return self.jobs.get(job_id)

    def list_stem_jobs(self) -> list[JobSnapshot]:
        return self.jobs.list(kind="stem_separation")

    def list_fingerprint_jobs(self) -> list[JobSnapshot]:
        return self.jobs.list(kind="fingerprint")

    def list_chipmunk_jobs(self) -> list[JobSnapshot]:
        return self.jobs.list(kind="chipmunk")

    def cancel(self, job_id: str) -> bool:
        return self.jobs.cancel(job_id)

    def _separate_job(
        self,
        context: JobContext,
        source: Path,
        output_format: str,
        title: str,
        callback: EventCallback | None,
    ) -> Mapping[str, Any]:
        with self._concurrency.slot(context):
            return self._separate_job_in_slot(
                context,
                source,
                output_format,
                title,
                callback,
            )

    def _separate_job_in_slot(
        self,
        context: JobContext,
        source: Path,
        output_format: str,
        title: str,
        callback: EventCallback | None,
    ) -> Mapping[str, Any]:
        if not self.config.separator_path:
            raise ProcessingError("audio-separator is not configured")
        model_path = self.config.separator_model_path
        model_directory = model_path.parent
        model_ready = model_path.is_file()
        working_directory = (
            self.config.cache_dir / "Processing" / "Stems" / context.job_id
        )
        working_directory.mkdir(parents=True, exist_ok=False)
        if not model_ready:
            model_directory.mkdir(parents=True, exist_ok=True)
        self.config.stems_dir.mkdir(parents=True, exist_ok=True)
        self._update(
            context,
            callback,
            event="processing.stems.progress",
            progress=8 if model_ready else 1,
            stage="Separating stems" if model_ready else "Preparing model",
            message="Starting local separation engine",
            phase="processing" if model_ready else "installing",
        )

        def on_line(raw_line: str) -> None:
            line = _ANSI_ESCAPE.sub("", raw_line).strip()
            if not line:
                return
            lower = line.lower()
            progress: float | None = None
            match = _PERCENT.search(line)
            if match:
                raw_progress = max(0.0, min(100.0, float(match.group(1))))
                progress = 10 + raw_progress * 0.84
            if "download" in lower and "model" in lower:
                stage = "Downloading model"
                phase = "installing"
            elif "load" in lower and "model" in lower:
                stage = "Loading model"
                phase = "processing"
            elif any(token in lower for token in ("separat", "infer", "process")):
                stage = "Separating stems"
                phase = "processing"
            else:
                stage = None
                phase = "processing"
            self._update(
                context,
                callback,
                event="processing.stems.progress",
                progress=progress,
                stage=stage,
                message=line[-300:],
                phase=phase,
            )

        command = [
            "--model_filename",
            model_path.name or DEFAULT_SEPARATOR_MODEL,
            "--model_file_dir",
            model_directory,
            "--output_dir",
            working_directory,
            "--output_format",
            output_format,
            "--sample_rate",
            "44100",
        ]
        if output_format == "MP3":
            command.extend(["--output_bitrate", "320k"])
        staged_input = (
            working_directory.with_name(f"{context.job_id}-input{source.suffix.lower()}")
            if _SHORT_SEPARATOR_INPUT
            else None
        )
        environment = {
            **os.environ,
            "AUDIO_SEPARATOR_MODEL_DIR": str(model_directory),
            "PYTHONUNBUFFERED": "1",
        }
        if self.config.ffmpeg_path:
            # audio-separator and pydub run `ffmpeg` from PATH, which neither a
            # Finder-launched app nor a typical Windows PC provides.
            environment["PATH"] = os.pathsep.join(
                filter(None, (str(Path(self.config.ffmpeg_path).parent), environment.get("PATH")))
            )
        try:
            if staged_input is not None:
                self.media.copy_file(source, staged_input, context=context)
            command.append(staged_input or source)
            self.media.run_executable(
                self.config.separator_path,
                [*self.config.separator_args, *command],
                timeout=None,
                context=context,
                line_callback=on_line,
                cwd=self.config.resource_dir,
                env=environment,
            )
            context.raise_if_cancelled()
            self._update(
                context,
                callback,
                event="processing.stems.progress",
                progress=95,
                stage="Publishing stems",
                message="Saving generated tracks",
                phase="publishing",
            )
            outputs = self._publish_stems(
                context,
                source,
                title,
                working_directory,
                output_format,
            )
            if not outputs:
                raise ProcessingError("The separator generated no output files")
            result = {
                "source": str(source),
                "output_format": output_format,
                "outputs": [output.to_dict() for output in outputs],
            }
            self._update(
                context,
                callback,
                event="processing.stems.ready",
                progress=100,
                stage="Completed",
                message=f"Generated {len(outputs)} stems",
                phase="completed",
                outputs=result["outputs"],
            )
            return result
        finally:
            shutil.rmtree(working_directory, ignore_errors=True)
            if staged_input is not None:
                staged_input.unlink(missing_ok=True)

    def _publish_stems(
        self,
        context: JobContext,
        source: Path,
        title: str,
        working_directory: Path,
        output_format: str,
    ) -> list[StemOutput]:
        allowed_extensions = set(self.separator_formats.values())
        generated = sorted(
            (
                path
                for path in working_directory.rglob("*")
                if path.is_file() and path.suffix.lower() in allowed_extensions
            ),
            key=lambda path: path.stat().st_mtime_ns,
        )
        outputs: list[StemOutput] = []
        for index, generated_path in enumerate(generated):
            context.raise_if_cancelled()
            stem, label = _stem_identity(generated_path.name, index)
            extension = generated_path.suffix.lower() or self.separator_formats[
                output_format
            ]
            reserved = reserve_unique_path(
                self.config.stems_dir,
                f"{safe_filename(source.stem)} - {label}{extension}",
            )
            publishing = reserved.with_name(
                f".{reserved.stem}.{context.job_id}.publishing{extension}"
            )
            try:
                self.media.copy_file(
                    generated_path,
                    publishing,
                    context=context,
                )
                atomic_publish(publishing, reserved)
            except Exception:
                publishing.unlink(missing_ok=True)
                reserved.unlink(missing_ok=True)
                raise
            metadata = {
                "title": f"{title} - {label}",
                "duration": self.media.duration(reserved, default=0),
                "type": "audio",
                "quality": output_format,
                "downloaded_at": time.time(),
                "playlist": "Separaciones",
                "source_file": source.name,
                "stem": stem,
                "separator_model": self.separator_model_label,
            }
            registration: Any = None
            try:
                registration = register_media(self.registrar, reserved, metadata)
            except Exception as exc:
                metadata["registration_error"] = str(exc)
            outputs.append(
                StemOutput(
                    label=label,
                    stem=stem,
                    filename=reserved.name,
                    local_path=str(reserved),
                    path=managed_content_path(
                        self.config.library_dir,
                        reserved,
                    ),
                    track_id=_registration_id(registration),
                    size_bytes=reserved.stat().st_size,
                )
            )
        return outputs

    def _fingerprint_job(
        self,
        context: JobContext,
        source: Path,
        title: str,
        callback: EventCallback | None,
        intensity: str,
    ) -> Mapping[str, Any]:
        with self._concurrency.slot(context):
            return self._fingerprint_job_in_slot(
                context,
                source,
                title,
                callback,
                intensity,
            )

    def _fingerprint_job_in_slot(
        self,
        context: JobContext,
        source: Path,
        title: str,
        callback: EventCallback | None,
        intensity: str,
    ) -> Mapping[str, Any]:
        self.config.protected_dir.mkdir(parents=True, exist_ok=True)
        # Lossless output avoids a second lossy encode of MP3/AAC/Opus inputs.
        extension = ".flac"
        reserved = reserve_unique_path(
            self.config.protected_dir,
            f"{safe_filename(source.stem)} - Protected{extension}",
        )
        publishing = reserved.with_name(
            f".{reserved.stem}.{context.job_id}.publishing{extension}"
        )
        try:
            probe = self.media.probe(source, context=context)
            stream = next((item for item in probe.get("streams", [])
                           if item.get("codec_type") == "audio"), {})
            sample_rate = int(stream.get("sample_rate") or 0)
            filters, profile = self.fingerprint_profile(context.job_id, intensity, sample_rate)
            if intensity == "quality":
                profile.update(
                    source_sample_rate=sample_rate,
                    separator_model=self.separator_model_label,
                )
            duration = max(
                self.media.duration(source, context=context, default=0),
                0,
            )
        except Exception:
            reserved.unlink(missing_ok=True)
            raise
        latest_progress = 2.0
        self._update(
            context,
            callback,
            event="processing.fingerprint.progress",
            progress=latest_progress,
            stage="Preparing",
            message="Preparing fingerprint variation",
            phase="processing",
            profile=profile,
        )

        def on_progress(line: str) -> None:
            nonlocal latest_progress
            key, separator, raw_value = line.partition("=")
            if not separator:
                return
            if key in {"out_time_ms", "out_time_us"} and duration > 0:
                try:
                    seconds = float(raw_value) / 1_000_000
                except ValueError:
                    return
                latest_progress = max(
                    latest_progress,
                    min(88.0, 4.0 + seconds * profile["speed"] / duration * 84.0),
                )
            elif key == "progress" and raw_value == "end":
                latest_progress = 89.0
            else:
                return
            self._update(
                context,
                callback,
                event="processing.fingerprint.progress",
                progress=latest_progress,
                stage="Processing audio",
                message=f"{latest_progress:.0f}%",
                phase="processing",
            )

        try:
            metadata = {
                "comment": f"YT Ultimate fingerprint variation {context.job_id}",
                "ytu_fingerprint_id": context.job_id,
            }
            if intensity == "quality":
                self._quality_fingerprint_audio(
                    context, source, publishing, profile, metadata, callback,
                )
            else:
                self.media.transcode_audio(
                    source,
                    publishing,
                    context=context,
                    filters=filters,
                    metadata=metadata,
                    progress_callback=on_progress,
                )
            context.raise_if_cancelled()
            self._update(
                context, callback, event="processing.fingerprint.progress",
                progress=90, stage="Verifying audio", message="Comparando huellas acústicas",
                phase="verifying",
            )
            verification = verify_audio_change(self.media, source, publishing, context)
            output_duration = self.media.duration(publishing, context=context, default=0)
            if output_duration <= 0:
                raise ProcessingError("No se pudo validar la duración del audio resultante")
            context.raise_if_cancelled()
            atomic_publish(publishing, reserved)
        except Exception:
            publishing.unlink(missing_ok=True)
            reserved.unlink(missing_ok=True)
            raise

        output_metadata = {
            "title": f"{title} - Protected",
            "duration": output_duration,
            "type": "audio",
            "quality": extension.lstrip(".").upper(),
            "downloaded_at": time.time(),
            "playlist": "Protegidos",
            "source_file": source.name,
            "fingerprint_id": context.job_id,
            "fingerprint_profile": profile,
            "fingerprint_verification": verification,
        }
        registration: Any = None
        try:
            registration = register_media(
                self.registrar,
                reserved,
                output_metadata,
            )
        except Exception as exc:
            output_metadata["registration_error"] = str(exc)
        output = {
            "filename": reserved.name,
            "local_path": str(reserved),
            "path": managed_content_path(
                self.config.library_dir,
                reserved,
            ),
            "track_id": _registration_id(registration),
            "size_bytes": reserved.stat().st_size,
            **output_metadata,
        }
        self._update(
            context,
            callback,
            event="processing.fingerprint.ready",
            progress=100,
            stage="Completed",
            message=reserved.name,
            phase="completed",
            output=output,
        )
        return output

    def _quality_fingerprint_audio(
        self,
        context: JobContext,
        source: Path,
        destination: Path,
        profile: dict[str, Any],
        metadata: Mapping[str, str],
        callback: EventCallback | None,
    ) -> None:
        # BS-RoFormer is trained at 44.1 kHz stereo. Its estimated vocal stem
        # stays untouched through the initial sum, then shares the explicitly
        # authorized gentle varispeed and common headroom gain with the music.
        import soundfile as sf

        if not self.config.separator_path:
            raise ProcessingError("CALIDAD requiere el motor de separación de voz")
        working = self.config.cache_dir / "Processing" / "Quality" / context.job_id
        stems = working / "stems"
        prepared = working / "input.wav"
        instrumental_output = working / "instrumental-modified.wav"
        mixed = working / "mixed.wav"
        before_remap = working / "mix-before-remap.flac"
        remapped = working / "remapped.wav"
        model_path = self.config.separator_model_path

        def update(progress: float, phase: str, message: str) -> None:
            self._update(
                context, callback, event="processing.fingerprint.progress",
                progress=progress, stage=message, message=message, phase=phase,
            )

        separation_progress = 6.0

        def separator_progress(raw_line: str) -> None:
            nonlocal separation_progress
            line = _ANSI_ESCAPE.sub("", raw_line).strip()
            if not line:
                return
            match = _PERCENT.search(line)
            if match:
                separation_progress = max(
                    separation_progress, 6 + min(100, float(match.group(1))) * 0.59,
                )
            update(separation_progress, "separating", line[-300:])

        environment = {
            **os.environ,
            "AUDIO_SEPARATOR_MODEL_DIR": str(model_path.parent),
            "PYTHONUNBUFFERED": "1",
        }
        if self.config.ffmpeg_path:
            environment["PATH"] = os.pathsep.join(filter(None, (
                str(Path(self.config.ffmpeg_path).parent), environment.get("PATH"),
            )))
        working.mkdir(parents=True, exist_ok=False)
        try:
            stems.mkdir()
            model_path.parent.mkdir(parents=True, exist_ok=True)
            update(3, "separating", "Preparando separación de voz e instrumental")
            # Soundfile inherits the input subtype. FLOAT input avoids the
            # separator's default pydub int16 conversion and lossy intermediates.
            self.media.run_ffmpeg(
                ["-y", "-hide_banner", "-nostdin", "-i", source,
                 "-map", "0:a:0", "-vn", "-ar", "44100", "-ac", "2",
                 "-c:a", "pcm_f32le", prepared],
                context=context,
            )
            original_info = sf.info(prepared)
            if original_info.frames <= 0:
                raise ProcessingError("La fuente no contiene audio para CALIDAD")
            self.media.run_executable(
                self.config.separator_path,
                [*self.config.separator_args,
                 "--model_filename", model_path.name or DEFAULT_SEPARATOR_MODEL,
                 "--model_file_dir", model_path.parent,
                 "--output_dir", stems, "--output_format", "WAV",
                 "--sample_rate", "44100", "--use_soundfile",
                 "--normalization", "1", "--amplification", "0",
                 "--custom_output_names", '{"Vocals":"vocals","Instrumental":"instrumental"}',
                 prepared],
                timeout=None, context=context, line_callback=separator_progress,
                cwd=self.config.resource_dir, env=environment,
            )
            context.raise_if_cancelled()
            vocals = stems / "vocals.wav"
            instrumental = stems / "instrumental.wav"
            for stem in (vocals, instrumental):
                if not stem.is_file():
                    # audio-separator omits near-silent stems. Do not silently
                    # substitute the full mix or claim a preserved vocal stem.
                    raise ProcessingError(
                        "CALIDAD necesita los dos stems de voz e instrumental; "
                        "el separador no generó ambos (puede haber un stem silencioso)"
                    )
                info = sf.info(stem)
                if (info.frames, info.samplerate, info.channels) != (
                    original_info.frames, 44100, 2,
                ):
                    raise ProcessingError("Los stems no conservan la duración y sincronía de la fuente")
            update(68, "processing", "Modificando solo la instrumental")
            from ytultimate.services.quality_audio import dynamic_instrumental_eq, shared_smooth_remap

            profile["instrumental_processing"] = dynamic_instrumental_eq(
                context, instrumental, instrumental_output,
                seed=context.job_id, max_gain_db=profile["max_spectral_gain_db"],
            )
            context.raise_if_cancelled()
            update(77, "recombining", "Uniendo la voz conservada con la instrumental")
            profile["instrumental_gain"] = self._mix_preserving_vocals(
                context, vocals, instrumental_output, mixed,
            )
            self.media.transcode_audio(
                mixed, before_remap, context=context, filters="aformat=sample_fmts=s32",
            )
            context.raise_if_cancelled()
            profile["pre_remap_vocal_integrity"] = self._verify_preserved_vocals(
                context, vocals, instrumental_output, before_remap,
                instrumental_gain=profile["instrumental_gain"],
            )
            profile["pre_remap_vocal_integrity"]["scope"] = "separated_stem_before_shared_remap"
            update(80, "remapping", "Aplicando cambios suaves conjuntos a voz e instrumental")
            profile["shared_processing"] = shared_smooth_remap(context, before_remap, remapped)
            profile["speed"] = profile["shared_processing"]["mean_speed_ratio"]
            profile["tempo_ratio"] = profile["speed"]
            self.media.transcode_audio(
                remapped, destination, context=context, filters="aformat=sample_fmts=s32",
                metadata=metadata,
            )
            context.raise_if_cancelled()
            update(86, "verifying", "Comprobando la mezcla final y el margen de audio")
            profile["final_audio_integrity"] = self._verify_shared_remap(context, remapped, destination)
        finally:
            shutil.rmtree(working, ignore_errors=True)

    @staticmethod
    def _verify_shared_remap(context: JobContext, remapped: Path, output: Path) -> dict[str, Any]:
        """Verify final PCM against the changed mix, not the original vocals."""
        import numpy as np
        import soundfile as sf
        from ytultimate.services.quality_audio import estimate_audio_true_peak

        tolerance = 2 ** -23
        maximum_error = 0.0
        with sf.SoundFile(remapped) as reference, sf.SoundFile(output) as final:
            expected = (reference.frames, reference.samplerate, reference.channels)
            if (reference.frames <= 0 or final.format != "FLAC" or final.subtype != "PCM_24"
                    or (final.frames, final.samplerate, final.channels) != expected):
                raise ProcessingError("No se pudo verificar la mezcla remapeada final")
            while True:
                context.raise_if_cancelled()
                samples = reference.read(65_536, dtype="float64", always_2d=True)
                if not len(samples):
                    break
                encoded = final.read(len(samples), dtype="float64", always_2d=True)
                if encoded.shape != samples.shape or not np.isfinite(encoded).all():
                    raise ProcessingError("La mezcla final contiene audio incompleto o inválido")
                error = encoded - samples
                if not np.isfinite(error).all():
                    raise ProcessingError("La mezcla remapeada contiene muestras inválidas")
                maximum_error = max(maximum_error, float(np.max(np.abs(error))))
                if maximum_error > tolerance:
                    raise ProcessingError("La exportación alteró la mezcla remapeada; no se guardó la copia")
        peak = estimate_audio_true_peak(context, output)
        if peak["peak_dbtp"] is not None and peak["peak_dbtp"] > -1.0:
            raise ProcessingError("La mezcla final no conserva el margen de pico requerido")
        return {
            "status": "verified", "scope": "processed_full_mix",
            "frames": expected[0], "sample_rate": expected[1], "channels": expected[2],
            "max_abs_error": maximum_error, "tolerance": tolerance,
            "output_format": "FLAC", "bit_depth": 24,
            "true_peak_method": peak["method"], "true_peak_estimate_dbtp": peak["peak_dbtp"],
            "max_allowed_true_peak_dbtp": -1.0,
        }

    @staticmethod
    def _verify_preserved_vocals(
        context: JobContext, vocals: Path, instrumental: Path, output: Path,
        *, instrumental_gain: float,
    ) -> dict[str, Any]:
        """Audit the initial encoded sum before the authorized shared remap."""
        import numpy as np
        import soundfile as sf

        tolerance = 2 ** -23  # One PCM24 quantization step, not a perceptual threshold.
        maximum_error = 0.0
        with (sf.SoundFile(vocals) as voice, sf.SoundFile(instrumental) as music,
              sf.SoundFile(output) as final):
            expected = (voice.frames, voice.samplerate, voice.channels)
            if (voice.frames <= 0 or final.format != "FLAC" or final.subtype != "PCM_24"
                    or (music.frames, music.samplerate, music.channels) != expected
                    or (final.frames, final.samplerate, final.channels) != expected
                    or not np.isfinite(instrumental_gain) or not 0 <= instrumental_gain <= 1):
                raise ProcessingError("No se pudo verificar la conservación de la voz en el FLAC final")
            while True:
                context.raise_if_cancelled()
                original_voice = voice.read(65_536, dtype="float64", always_2d=True)
                if not len(original_voice):
                    break
                accompaniment = music.read(len(original_voice), dtype="float64", always_2d=True)
                final_samples = final.read(len(original_voice), dtype="float64", always_2d=True)
                if accompaniment.shape != original_voice.shape or final_samples.shape != original_voice.shape:
                    raise ProcessingError("La verificación de voz encontró audio incompleto")
                residual = final_samples - instrumental_gain * accompaniment - original_voice
                if not np.isfinite(residual).all():
                    raise ProcessingError("La verificación de voz encontró muestras inválidas")
                maximum_error = max(maximum_error, float(np.max(np.abs(residual))))
                if maximum_error > tolerance:
                    raise ProcessingError("La mezcla previa alteró la voz separada; no se guardó la copia")
            return {
                "status": "verified", "scope": "separated_stem",
                "frames": voice.frames, "channels": voice.channels,
                "sample_rate": voice.samplerate, "max_abs_error": maximum_error,
                "tolerance": tolerance, "output_format": "FLAC", "bit_depth": 24,
            }

    @staticmethod
    def _mix_preserving_vocals(
        context: JobContext, vocals: Path, instrumental: Path, destination: Path,
    ) -> float:
        import numpy as np
        import soundfile as sf

        block_size = 65_536
        gain = 1.0
        with sf.SoundFile(vocals) as voice, sf.SoundFile(instrumental) as music:
            if (voice.frames, voice.samplerate, voice.channels) != (
                music.frames, music.samplerate, music.channels,
            ) or voice.frames <= 0:
                raise ProcessingError("No se pueden unir stems con distinta duración o formato")
            # One constant gain on the instrumental prevents clipping without
            # normalizing, limiting, ducking, or otherwise processing the voice.
            while True:
                context.raise_if_cancelled()
                vocal_samples = voice.read(block_size, dtype="float64", always_2d=True)
                music_samples = music.read(block_size, dtype="float64", always_2d=True)
                if not len(vocal_samples):
                    break
                if (not np.isfinite(vocal_samples).all() or
                        not np.isfinite(music_samples).all() or
                        np.max(np.abs(vocal_samples)) > 1):
                    raise ProcessingError("Los stems contienen muestras inválidas o una voz saturada")
                positive = music_samples > 0
                negative = music_samples < 0
                if positive.any():
                    gain = min(gain, float(np.min(
                        (1.0 - vocal_samples[positive]) / music_samples[positive],
                    )))
                if negative.any():
                    gain = min(gain, float(np.min(
                        (-1.0 - vocal_samples[negative]) / music_samples[negative],
                    )))
            # A saturated vocal peak can otherwise force a whole song's
            # accompaniment to silence. Never publish that as a quality mix.
            if gain < 0.05:
                raise ProcessingError(
                    "La voz no deja margen suficiente para unir la instrumental sin saturación "
                    "y sin alterar la voz. Prueba otra fuente para CALIDAD."
                )
            if gain < 1:
                gain *= 1 - 1e-7
            voice.seek(0)
            music.seek(0)
            # DOUBLE is an intermediate only. Round once, at final PCM24 export.
            with sf.SoundFile(
                destination, "w", samplerate=voice.samplerate, channels=voice.channels,
                format="WAV", subtype="DOUBLE",
            ) as output:
                while True:
                    context.raise_if_cancelled()
                    vocal_samples = voice.read(block_size, dtype="float64", always_2d=True)
                    music_samples = music.read(block_size, dtype="float64", always_2d=True)
                    if not len(vocal_samples):
                        break
                    output.write(vocal_samples + gain * music_samples)
        return gain

    def _chipmunk_job(
        self,
        context: JobContext,
        source: Path,
        title: str,
        filters: str,
        profile: Mapping[str, Any],
        callback: EventCallback | None,
    ) -> Mapping[str, Any]:
        with self._concurrency.slot(context):
            return self._chipmunk_job_in_slot(
                context,
                source,
                title,
                filters,
                profile,
                callback,
            )

    def _chipmunk_job_in_slot(
        self,
        context: JobContext,
        source: Path,
        title: str,
        filters: str,
        profile: Mapping[str, Any],
        callback: EventCallback | None,
    ) -> Mapping[str, Any]:
        output_dir = self.config.chipmunk_dir
        output_dir.mkdir(parents=True, exist_ok=True)
        extension = source.suffix.lower()
        if extension not in SUPPORTED_AUDIO_EXTENSIONS:
            # Video sources keep only their audio track.
            extension = ".m4a"
        reserved = reserve_unique_path(
            output_dir,
            f"{safe_filename(source.stem)} - Ardilla{extension}",
        )
        publishing = reserved.with_name(
            f".{reserved.stem}.{context.job_id}.publishing{extension}"
        )
        try:
            source_duration = max(
                self.media.duration(source, context=context, default=0),
                0,
            )
        except Exception:
            reserved.unlink(missing_ok=True)
            raise
        # The output timeline is shorter or longer by the speed factor; ffmpeg
        # reports progress against the output.
        speed = float(profile.get("speed") or 1.0)
        expected = source_duration / speed if speed > 0 else source_duration
        latest_progress = 2.0
        self._update(
            context,
            callback,
            event="processing.chipmunk.progress",
            progress=latest_progress,
            stage="Preparing",
            message="Preparing chipmunk version",
            phase="processing",
        )

        def on_progress(line: str) -> None:
            nonlocal latest_progress
            key, separator, raw_value = line.partition("=")
            if not separator:
                return
            if key in {"out_time_ms", "out_time_us"} and expected > 0:
                try:
                    seconds = float(raw_value) / 1_000_000
                except ValueError:
                    return
                latest_progress = max(
                    latest_progress,
                    min(96.0, 4.0 + seconds / expected * 92.0),
                )
            elif key == "progress" and raw_value == "end":
                latest_progress = 97.0
            else:
                return
            self._update(
                context,
                callback,
                event="processing.chipmunk.progress",
                progress=latest_progress,
                stage="Processing audio",
                message=f"{latest_progress:.0f}%",
                phase="processing",
            )

        try:
            self.media.transcode_audio(
                source,
                publishing,
                context=context,
                filters=filters,
                metadata={
                    "title": f"{title} (Ardilla)",
                    "comment": "YT Ultimate chipmunk version",
                },
                progress_callback=on_progress,
            )
            context.raise_if_cancelled()
            atomic_publish(publishing, reserved)
        except Exception:
            publishing.unlink(missing_ok=True)
            reserved.unlink(missing_ok=True)
            raise

        output_metadata = {
            "title": f"{title} (Ardilla)",
            "duration": self.media.duration(reserved, default=0)
            or expected,
            "type": "audio",
            "quality": extension.lstrip(".").upper(),
            "downloaded_at": time.time(),
            "playlist": "Ardillas",
            "source_file": source.name,
            "chipmunk_profile": dict(profile),
        }
        registration: Any = None
        try:
            registration = register_media(
                self.registrar,
                reserved,
                output_metadata,
            )
        except Exception as exc:
            output_metadata["registration_error"] = str(exc)
        output = {
            "filename": reserved.name,
            "local_path": str(reserved),
            "path": managed_content_path(
                self.config.library_dir,
                reserved,
            ),
            "track_id": _registration_id(registration),
            "size_bytes": reserved.stat().st_size,
            **output_metadata,
        }
        self._update(
            context,
            callback,
            event="processing.chipmunk.ready",
            progress=100,
            stage="Completed",
            message=reserved.name,
            phase="completed",
            output=output,
        )
        return output

    @staticmethod
    def chipmunk_profile(
        pitch_semitones: float = 7.0,
        speed: float = 1.0,
        brightness_db: float = 3.0,
    ) -> tuple[str, dict[str, Any]]:
        """Raise pitch (and formants, which makes the cartoon voice).

        ``asetrate`` shifts pitch and formants together but also speeds the
        audio up; ``atempo`` then brings the tempo to the requested speed.
        """
        pitch = max(1.0, min(12.0, float(pitch_semitones)))
        speed = max(0.5, min(2.0, float(speed)))
        brightness = max(0.0, min(12.0, float(brightness_db)))
        ratio = 2.0 ** (pitch / 12.0)
        rate_hz = round(44_100 * ratio)
        stages = [
            "aresample=44100",
            f"asetrate={rate_hz}",
            "aresample=44100:dither_method=triangular",
            *(f"atempo={factor:.6f}" for factor in _atempo_chain(speed / ratio)),
        ]
        if brightness > 0:
            stages.append(f"treble=g={brightness:.2f}:f=3000:t=s:w=0.7")
        stages.append("alimiter=limit=0.97:level=disabled")
        return ",".join(stages), {
            "pitch_semitones": round(pitch, 2),
            "speed": round(speed, 3),
            "brightness_db": round(brightness, 2),
        }

    @staticmethod
    def fingerprint_profile(
        identifier: str, intensity: str = "strong", sample_rate: int = 44_100,
    ) -> tuple[str, dict[str, Any]]:
        if intensity not in {"balanced", "strong", "quality"}:
            raise ValueError("Fingerprint intensity must be balanced, strong or quality")
        if not 8_000 <= sample_rate <= 384_000:
            raise ValueError("Unsupported source sample rate for fingerprint processing")
        seed = int(hashlib.sha256(identifier.encode("utf-8")).hexdigest()[:8], 16)
        if intensity == "quality":
            # The initial instrumental EQ is followed by one gentle varispeed
            # map and one constant headroom gain shared by vocals and music.
            # These bounds are not a universal recognition guarantee.
            return "", {
                "version": 6,
                "intensity": "quality",
                "sample_rate": 44_100,
                "output_format": "FLAC",
                "pitch_semitones": 0.28,
                "pitch_range_semitones": [0.20, 0.36],
                "speed": 2 ** (0.28 / 12),
                "rate_hz": 44_100,
                "tempo_ratio": 2 ** (0.28 / 12),
                "instrumental_method": "dynamic_spectral_eq",
                "max_spectral_gain_db": 6.0,
                "processing_target": "instrumental_then_full_mix",
                "vocals_preserved": False,
                "vocal_processing": "shared_smooth_remap",
            }
        # Version 1 used <= 0.02% pitch changes and near-flat EQ; that mostly
        # changed file bytes. Version 2 changes spectral and temporal features.
        # These are engineering presets, not a guarantee against identification.
        pitch = (1.75 if intensity == "strong" else 1.0) + (seed % 21) / 100.0
        speed = 1.025 + ((seed // 21) % 11) / 1000.0
        rate_hz = round(sample_rate * 2 ** (pitch / 12.0))
        tempo_ratio = speed / (rate_hz / sample_rate)
        low_gain = -0.6 - ((seed // 701) % 5) / 10.0
        high_gain = 0.6 + ((seed // 1301) % 5) / 10.0
        filters = ",".join(
            [
                "aformat=sample_fmts=fltp",
                f"asetrate={rate_hz}",
                f"aresample={sample_rate}:filter_size=64:phase_shift=10",
                f"atempo={tempo_ratio:.7f}",
                f"equalizer=f=180:t=q:w=0.8:g={low_gain:.3f}",
                f"equalizer=f=4200:t=q:w=1.1:g={high_gain:.3f}",
                "alimiter=limit=0.97:level=disabled:latency=1",
                "aformat=sample_fmts=s32",
            ]
        )
        return filters, {
            "version": 3,
            "intensity": intensity,
            "sample_rate": sample_rate,
            "output_format": "FLAC",
            "pitch_semitones": round(pitch, 2),
            "speed": round(speed, 3),
            "rate_hz": rate_hz,
            "tempo_ratio": round(tempo_ratio, 7),
            "low_gain_db": round(low_gain, 3),
            "high_gain_db": round(high_gain, 3),
        }

    def _resolve_source(
        self,
        source: Path | str,
        *,
        audio_only: bool = False,
    ) -> Path:
        value = Path(source).expanduser()
        if not value.is_absolute():
            value = self.config.library_dir / value
        resolved = value.resolve()
        if not self.allow_external_sources:
            root = self.config.library_dir.expanduser().resolve()
            try:
                resolved.relative_to(root)
            except ValueError as exc:
                raise ValueError("Processing source must be inside the library") from exc
        resolved = self.media.require_media_file(resolved)
        if audio_only and resolved.suffix.lower() not in SUPPORTED_AUDIO_EXTENSIONS:
            raise ValueError("Fingerprint processing only supports audio files")
        return resolved

    def _update(
        self,
        context: JobContext,
        callback: EventCallback | None,
        *,
        event: str,
        progress: float | None = None,
        stage: str | None = None,
        message: str | None = None,
        **metadata: Any,
    ) -> JobSnapshot:
        snapshot = context.update(
            progress=progress,
            stage=stage,
            message=message,
            metadata=metadata,
        )
        self._emit(event, snapshot.to_dict(), callback)
        return snapshot

    def _emit(
        self,
        event: str,
        payload: Mapping[str, Any],
        callback: EventCallback | None,
    ) -> None:
        for target in (self.events, callback):
            dispatch_event(target, event, payload)


def _normalize_extension(value: str) -> str:
    extension = str(value).strip().lower()
    if not extension.startswith("."):
        extension = f".{extension}"
    if extension not in SUPPORTED_AUDIO_EXTENSIONS:
        raise ValueError(f"Unsupported audio extension: {extension}")
    return extension


def _atempo_chain(factor: float) -> list[float]:
    """Split a tempo factor into steps every ffmpeg build accepts (0.5-2.0)."""
    if factor <= 0:
        raise ValueError("Tempo factor must be positive")
    steps: list[float] = []
    while factor > 2.0:
        steps.append(2.0)
        factor /= 2.0
    while factor < 0.5:
        steps.append(0.5)
        factor /= 0.5
    if abs(factor - 1.0) > 1e-6 or not steps:
        steps.append(factor)
    return steps


def _stem_identity(filename: str, index: int) -> tuple[str, str]:
    normalized = filename.lower()
    if "vocal" in normalized or "voice" in normalized:
        return "vocals", "Vocals"
    if (
        "instrument" in normalized
        or "no_vocal" in normalized
        or "karaoke" in normalized
    ):
        return "instrumental", "Instrumental"
    return f"stem_{index + 1}", f"Stem {index + 1}"


def _registration_id(value: Any) -> str:
    return str(value.get("id") or "") if isinstance(value, Mapping) else ""
