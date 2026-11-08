from __future__ import annotations

from pathlib import Path
from typing import Any

from fastapi import APIRouter

from ...jobs import JobSnapshot
from ...schemas import ChipmunkRequest, FingerprintRequest, ProcessingRequest, SeparationRequest
from ...services.library import LibraryService
from ...services.processing import ProcessingService
from ..dependencies import ContainerDependency
from ..presentation import public_job


router = APIRouter(tags=["processing"])


def _job(snapshot: JobSnapshot[Any]) -> dict[str, Any]:
    return public_job(snapshot)


def _source_path(
    payload: ProcessingRequest,
    library: LibraryService,
) -> Path:
    if payload.track_id:
        return library.resolve_content(payload.track_id)
    if payload.filename:
        matches = library.find_by_filename(payload.filename)
        if len(matches) == 1:
            return library.resolve_content(matches[0]["id"])
        if len(matches) > 1:
            raise ValueError(
                "Filename is ambiguous; provide the stable track_id"
            )
    if payload.local_path:
        return Path(payload.local_path)
    raise ValueError("A valid track_id, filename, or local_path is required")


@router.get("/separator/info")
def separator_info(
    container: ContainerDependency,
) -> dict[str, Any]:
    return container.require(
        "processing",
        ProcessingService,
    ).separator_info()


@router.get("/separator/jobs")
def separator_jobs(
    container: ContainerDependency,
) -> list[dict[str, Any]]:
    service = container.require("processing", ProcessingService)
    return [_job(snapshot) for snapshot in service.list_stem_jobs()]


@router.post("/separator/jobs")
def start_separator_job(
    payload: SeparationRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    library = container.require("library", LibraryService)
    processing = container.require("processing", ProcessingService)
    snapshot = processing.start_stem_separation(
        _source_path(payload, library),
        output_format=payload.output_format,
        title=payload.title or None,
    )
    return _job(snapshot)


@router.get("/separator/jobs/{job_id}")
def get_separator_job(
    job_id: str,
    container: ContainerDependency,
) -> dict[str, Any]:
    snapshot = container.require(
        "processing",
        ProcessingService,
    ).get(job_id)
    if snapshot.kind != "stem_separation":
        raise ValueError("Job is not a stem-separation job")
    return _job(snapshot)


@router.delete("/separator/jobs/{job_id}")
def cancel_separator_job(
    job_id: str,
    container: ContainerDependency,
) -> dict[str, Any]:
    processing = container.require("processing", ProcessingService)
    snapshot = processing.get(job_id)
    if snapshot.kind != "stem_separation":
        raise ValueError("Job is not a stem-separation job")
    cancelled = processing.cancel(job_id)
    return {"status": "cancelling" if cancelled else "finished", "cancelled": cancelled}


@router.get("/fingerprint/jobs")
def fingerprint_jobs(
    container: ContainerDependency,
) -> list[dict[str, Any]]:
    service = container.require("processing", ProcessingService)
    return [_job(snapshot) for snapshot in service.list_fingerprint_jobs()]


@router.post("/fingerprint/jobs")
def start_fingerprint_job(
    payload: FingerprintRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    library = container.require("library", LibraryService)
    processing = container.require("processing", ProcessingService)
    snapshot = processing.start_fingerprint(
        _source_path(payload, library),
        title=payload.title or None,
        intensity=payload.intensity,
    )
    return _job(snapshot)


@router.get("/fingerprint/jobs/{job_id}")
def get_fingerprint_job(
    job_id: str,
    container: ContainerDependency,
) -> dict[str, Any]:
    snapshot = container.require(
        "processing",
        ProcessingService,
    ).get(job_id)
    if snapshot.kind != "fingerprint":
        raise ValueError("Job is not a fingerprint job")
    return _job(snapshot)


@router.delete("/fingerprint/jobs/{job_id}")
def cancel_fingerprint_job(
    job_id: str,
    container: ContainerDependency,
) -> dict[str, Any]:
    processing = container.require("processing", ProcessingService)
    snapshot = processing.get(job_id)
    if snapshot.kind != "fingerprint":
        raise ValueError("Job is not a fingerprint job")
    cancelled = processing.cancel(job_id)
    return {"status": "cancelling" if cancelled else "finished", "cancelled": cancelled}


@router.get("/chipmunk/jobs")
def chipmunk_jobs(
    container: ContainerDependency,
) -> list[dict[str, Any]]:
    service = container.require("processing", ProcessingService)
    return [_job(snapshot) for snapshot in service.list_chipmunk_jobs()]


@router.post("/chipmunk/jobs")
def start_chipmunk_job(
    payload: ChipmunkRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    library = container.require("library", LibraryService)
    processing = container.require("processing", ProcessingService)
    snapshot = processing.start_chipmunk(
        _source_path(payload, library),
        pitch_semitones=payload.pitch_semitones,
        speed=payload.speed,
        brightness_db=payload.brightness_db,
        title=payload.title or None,
    )
    return _job(snapshot)


@router.get("/chipmunk/jobs/{job_id}")
def get_chipmunk_job(
    job_id: str,
    container: ContainerDependency,
) -> dict[str, Any]:
    snapshot = container.require(
        "processing",
        ProcessingService,
    ).get(job_id)
    if snapshot.kind != "chipmunk":
        raise ValueError("Job is not a chipmunk job")
    return _job(snapshot)


__all__ = ["router"]
