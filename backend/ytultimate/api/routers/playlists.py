from __future__ import annotations

from typing import Any

from fastapi import APIRouter

from ...schemas import (
    LegacyPlaylistTrackRequest,
    PlaylistCreateRequest,
)
from ...services.library import LibraryService
from ...services.playlists import PlaylistService
from ..dependencies import ContainerDependency
from ..models import (
    PlaylistMembershipRequest,
    PlaylistOrderRequest,
    PlaylistUpdateRequest,
)
from .library import _public_track


router = APIRouter(prefix="/playlists", tags=["playlists"])


def _detailed_playlist(
    playlist: dict[str, Any],
    library: LibraryService,
) -> dict[str, Any]:
    detailed = dict(playlist)
    tracks: list[dict[str, Any]] = []
    for membership in playlist.get("tracks", []):
        try:
            track = _public_track(
                library.get_track(
                    membership["id"],
                    include_missing=True,
                )
            )
        except LookupError:
            track = dict(membership)
        track["position"] = membership.get("position")
        track["added_at"] = membership.get("added_at")
        tracks.append(track)
    detailed["tracks"] = tracks
    return detailed


@router.get("")
def legacy_playlists(
    container: ContainerDependency,
) -> dict[str, list[str]]:
    """Filename map consumed by the simple library and playlist screens."""

    return container.require("playlists", PlaylistService).as_legacy_map()


@router.post("")
def create_playlist(
    payload: PlaylistCreateRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    return container.require("playlists", PlaylistService).create(payload.name)


@router.post("/add")
def legacy_add_track(
    payload: LegacyPlaylistTrackRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    identifier = payload.track_id or payload.filename
    return container.require("playlists", PlaylistService).add_by_name(
        payload.playlist_name,
        identifier,
    )


@router.post("/remove")
def legacy_remove_track(
    payload: LegacyPlaylistTrackRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    identifier = payload.track_id or payload.filename
    removed = container.require(
        "playlists",
        PlaylistService,
    ).remove_by_name(payload.playlist_name, identifier)
    return {"status": "removed" if removed else "not_found", "removed": removed}


@router.get("/details")
def playlist_details(
    container: ContainerDependency,
) -> dict[str, Any]:
    playlists = container.require("playlists", PlaylistService)
    library = container.require("library", LibraryService)
    items = [
        _detailed_playlist(item, library)
        for item in playlists.list(include_tracks=True)
    ]
    return {"items": items, "count": len(items)}


@router.get("/{playlist_id}")
def get_playlist(
    playlist_id: str,
    container: ContainerDependency,
) -> dict[str, Any]:
    playlists = container.require("playlists", PlaylistService)
    library = container.require("library", LibraryService)
    return _detailed_playlist(playlists.get(playlist_id), library)


@router.patch("/{playlist_id}")
def update_playlist(
    playlist_id: str,
    payload: PlaylistUpdateRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    values = payload.model_dump(exclude_none=True)
    return container.require("playlists", PlaylistService).update(
        playlist_id,
        **values,
    )


@router.delete("/{playlist_id}")
def delete_playlist(
    playlist_id: str,
    container: ContainerDependency,
) -> dict[str, Any]:
    deleted = container.require("playlists", PlaylistService).delete(
        playlist_id
    )
    return {"status": "deleted", "deleted": deleted}


@router.post("/{playlist_id}/tracks")
def add_playlist_track(
    playlist_id: str,
    payload: PlaylistMembershipRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    return container.require("playlists", PlaylistService).add_track(
        playlist_id,
        payload.track_id,
        position=payload.position,
    )


@router.delete("/{playlist_id}/tracks/{track_id}")
def remove_playlist_track(
    playlist_id: str,
    track_id: str,
    container: ContainerDependency,
) -> dict[str, Any]:
    removed = container.require("playlists", PlaylistService).remove_track(
        playlist_id,
        track_id,
    )
    return {"status": "removed" if removed else "not_found", "removed": removed}


@router.put("/{playlist_id}/tracks/order")
def reorder_playlist_tracks(
    playlist_id: str,
    payload: PlaylistOrderRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    return container.require("playlists", PlaylistService).reorder_tracks(
        playlist_id,
        payload.track_ids,
    )


__all__ = ["router"]
