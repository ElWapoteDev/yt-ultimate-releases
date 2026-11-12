from __future__ import annotations

from typing import Any

from fastapi import APIRouter

from ...jobs import JobSnapshot
from ...schemas import DownloadRequest, PlaylistDownloadRequest
from ...services.downloads import DownloadService
from ..dependencies import ContainerDependency
from ..models import DownloadOptionsRequest
from ..presentation import public_job


router = APIRouter(tags=["downloads"])


def _job(snapshot: JobSnapshot[Any]) -> dict[str, Any]:
    return public_job(snapshot)


def _download_job(
    service: DownloadService,
    job_id: str,
) -> JobSnapshot[Any]:
    snapshot = service.get(job_id)
    if snapshot.kind not in {"download", "playlist_download"}:
        raise ValueError("Job is not a download job")
    return snapshot


def _start_download(
    service: DownloadService,
    *,
    url: str,
    media_type: str,
    quality: str | None = None,
    title: str = "",
    thumbnail: str = "",
) -> dict[str, Any]:
    return _job(
        service.start_download(
            url,
            media_type,
            quality=quality,
            title=title or None,
            thumbnail=thumbnail or None,
        )
    )


@router.post("/download_quick")
def download_quick(
    payload: DownloadRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    return _start_download(
        container.require("downloads", DownloadService),
        url=payload.url,
        media_type=payload.type.value,
        title=payload.title,
        thumbnail=payload.thumbnail,
    )


@router.post("/download_playlist")
def download_playlist(
    payload: PlaylistDownloadRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    snapshot = container.require(
        "downloads",
        DownloadService,
    ).start_playlist(payload.url, payload.type.value)
    return _job(snapshot)


@router.post("/downloads")
def create_download(
    payload: DownloadOptionsRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    return _start_download(
        container.require("downloads", DownloadService),
        url=payload.url,
        media_type=payload.type,
        quality=payload.quality,
        title=payload.title,
        thumbnail=payload.thumbnail,
    )


@router.get("/downloads")
def list_downloads(
    container: ContainerDependency,
) -> list[dict[str, Any]]:
    return [
        _job(snapshot)
        for snapshot in container.require(
            "downloads",
            DownloadService,
        ).list()
    ]


@router.get("/downloads/{job_id}")
def get_download(
    job_id: str,
    container: ContainerDependency,
) -> dict[str, Any]:
    service = container.require("downloads", DownloadService)
    return _job(
        _download_job(service, job_id)
    )


@router.post("/downloads/{job_id}/cancel")
def cancel_download(
    job_id: str,
    container: ContainerDependency,
) -> dict[str, Any]:
    service = container.require(
        "downloads",
        DownloadService,
    )
    _download_job(service, job_id)
    cancelled = service.cancel(job_id)
    return {"status": "cancelling" if cancelled else "finished", "cancelled": cancelled}


@router.delete("/downloads/{job_id}")
def dismiss_download(
    job_id: str,
    container: ContainerDependency,
) -> dict[str, Any]:
    dismissed = container.require(
        "downloads",
        DownloadService,
    ).dismiss(job_id)
    return {"status": "dismissed" if dismissed else "unchanged", "dismissed": dismissed}


@router.post("/downloads/clear_completed")
def clear_completed_downloads(
    container: ContainerDependency,
) -> dict[str, Any]:
    cleared = container.require(
        "downloads",
        DownloadService,
    ).clear_finished()
    return {"status": "cleared", "cleared": cleared}


@router.delete("/downloads")
def clear_download_history(
    container: ContainerDependency,
) -> dict[str, Any]:
    return clear_completed_downloads(container)


__all__ = ["router"]
