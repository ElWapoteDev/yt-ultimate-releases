from __future__ import annotations

import asyncio
import mimetypes
from typing import Any

from fastapi import APIRouter, Query
from fastapi.responses import FileResponse

from ...schemas import LibraryImportRequest
from ...services.library import LibraryService, TrackNotFoundError
from ...services.playlists import (
    PlaylistConflictError,
    PlaylistService,
)
from ..dependencies import ContainerDependency


router = APIRouter(prefix="/library", tags=["library"])


def _public_track(track: dict[str, Any]) -> dict[str, Any]:
    identifier = str(track["id"])
    public = dict(track)
    public["path"] = f"/api/v1/library/tracks/{identifier}/content"
    if public.get("artwork_url"):
        public["thumbnail"] = public["artwork_url"]
    elif public.get("thumbnail"):
        public["thumbnail"] = (
            f"/api/v1/library/tracks/{identifier}/artwork"
        )
    else:
        public["thumbnail"] = ""
    return public


def _resolve_identifier(
    service: LibraryService,
    identifier: str,
) -> dict[str, Any]:
    try:
        return service.get_track(identifier)
    except TrackNotFoundError:
        matches = service.find_by_filename(identifier)
        if not matches:
            raise
        if len(matches) > 1:
            raise ValueError(
                "Filename is ambiguous; delete the track by its stable ID"
            )
        return matches[0]


@router.get("")
async def list_library(
    container: ContainerDependency,
    include_missing: bool = False,
    refresh: bool = False,
    q: str = "",
    media_type: str | None = None,
    limit: int | None = Query(default=None, ge=1, le=10_000),
    offset: int = Query(default=0, ge=0),
    order: str = "newest",
) -> list[dict[str, Any]]:
    service = container.require("library", LibraryService)
    tracks = await asyncio.to_thread(
        service.list_tracks,
        include_missing=include_missing,
        refresh=refresh,
        query=q,
        media_type=media_type,
        limit=limit,
        offset=offset,
        order=order,
    )
    return [_public_track(track) for track in tracks]


@router.post("/import")
async def import_library(
    payload: LibraryImportRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    library = container.require("library", LibraryService)
    result = await asyncio.to_thread(
        library.import_paths,
        payload.paths,
        mode=payload.mode.value,
        recursive=True,
        probe=True,
    )
    imported_tracks = [
        _public_track(track) for track in result.get("tracks", [])
    ]

    playlist_name = payload.playlist_name.strip()
    playlist_memberships: dict[str, int] = {}
    requested_playlists = (
        {playlist_name: [track["id"] for track in imported_tracks]}
        if playlist_name
        else result.get("playlists", {})
    )
    if requested_playlists:
        playlists = container.require("playlists", PlaylistService)
        for requested_name, track_ids in requested_playlists.items():
            if not track_ids:
                continue
            try:
                playlist = playlists.create(requested_name)
            except PlaylistConflictError:
                playlist = playlists.get(requested_name)
            for track_id in track_ids:
                playlists.add_track(playlist["id"], track_id)
            playlist_memberships[requested_name] = len(track_ids)

    return {
        **result,
        "imported": imported_tracks,
        "imported_count": len(imported_tracks),
        "skipped_count": int(result.get("skipped", 0)),
        "playlist_name": playlist_name,
        "playlist_memberships": playlist_memberships,
    }


@router.get("/stats")
async def library_stats(
    container: ContainerDependency,
    refresh: bool = False,
) -> dict[str, Any]:
    service = container.require("library", LibraryService)
    return await asyncio.to_thread(service.stats, refresh=refresh)


@router.post("/clean")
async def clean_library(
    container: ContainerDependency,
) -> dict[str, Any]:
    service = container.require("library", LibraryService)
    result = await asyncio.to_thread(service.clean_temporary_files)
    reconcile = await asyncio.to_thread(service.reconcile, probe=False)
    return {**result, "reconcile": reconcile}


@router.get("/tracks/{track_id}/content")
@router.head("/tracks/{track_id}/content")
def track_content(
    track_id: str,
    container: ContainerDependency,
) -> FileResponse:
    service = container.require("library", LibraryService)
    path = service.resolve_content(track_id)
    return FileResponse(
        path,
        media_type=mimetypes.guess_type(path.name)[0]
        or "application/octet-stream",
        headers={
            "Accept-Ranges": "bytes",
            "Cache-Control": "private, no-cache",
            "X-Content-Type-Options": "nosniff",
        },
    )


@router.get("/tracks/{track_id}/artwork")
@router.head("/tracks/{track_id}/artwork")
def track_artwork(
    track_id: str,
    container: ContainerDependency,
) -> FileResponse:
    service = container.require("library", LibraryService)
    path = service.resolve_artwork(track_id)
    if path is None:
        raise FileNotFoundError(f"Artwork not found for track: {track_id}")
    return FileResponse(
        path,
        media_type=mimetypes.guess_type(path.name)[0] or "image/jpeg",
        headers={
            "Cache-Control": "private, max-age=3600",
            "X-Content-Type-Options": "nosniff",
        },
    )


@router.delete("/{identifier}")
async def delete_library_track(
    identifier: str,
    container: ContainerDependency,
    delete_external: bool = False,
) -> dict[str, Any]:
    service = container.require("library", LibraryService)
    track = await asyncio.to_thread(
        _resolve_identifier,
        service,
        identifier,
    )
    return await asyncio.to_thread(
        service.delete_track,
        track["id"],
        delete_file=True,
        delete_external=delete_external,
    )


__all__ = ["router"]
