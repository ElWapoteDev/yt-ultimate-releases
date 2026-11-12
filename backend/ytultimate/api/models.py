"""Transport-only request models not shared with the service layer."""

from __future__ import annotations

from typing import Any, Annotated

from pydantic import Field

from ..schemas import NonEmptyText, StrictModel


class SettingsResetRequest(StrictModel):
    keys: list[str] | None = None


class PlaylistUpdateRequest(StrictModel):
    name: Annotated[str | None, Field(min_length=1, max_length=240)] = None
    description: Annotated[str | None, Field(max_length=1_000)] = None
    artwork_path: Annotated[str | None, Field(max_length=4_096)] = None


class PlaylistMembershipRequest(StrictModel):
    track_id: NonEmptyText
    position: Annotated[int | None, Field(ge=0)] = None


class PlaylistOrderRequest(StrictModel):
    track_ids: Annotated[list[str], Field(min_length=1, max_length=10_000)]


class DownloadOptionsRequest(StrictModel):
    url: Annotated[str, Field(min_length=8, max_length=2_048)]
    type: str = "audio"
    quality: Annotated[str | None, Field(max_length=32)] = None
    title: Annotated[str, Field(max_length=500)] = ""
    thumbnail: Annotated[str, Field(max_length=2_048)] = ""


class PluginConfigRequest(StrictModel):
    config: dict[str, Any] = Field(default_factory=dict)
    replace: bool = False


__all__ = [
    "DownloadOptionsRequest",
    "PlaylistMembershipRequest",
    "PlaylistOrderRequest",
    "PlaylistUpdateRequest",
    "PluginConfigRequest",
    "SettingsResetRequest",
]
