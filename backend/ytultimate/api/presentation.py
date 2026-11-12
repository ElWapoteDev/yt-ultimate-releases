"""Canonical transport projections for service-layer values."""

from __future__ import annotations

from typing import Any

from ..jobs import JobSnapshot


def with_content_links(value: Any) -> Any:
    """Replace internal/legacy media paths wherever a stable track ID exists."""

    if isinstance(value, dict):
        linked = {
            key: with_content_links(item)
            for key, item in value.items()
        }
        track_id = str(linked.get("track_id") or "")
        if track_id:
            path = f"/api/v1/library/tracks/{track_id}/content"
            linked["path"] = path
            linked["content_url"] = path
        return linked
    if isinstance(value, (list, tuple)):
        return [with_content_links(item) for item in value]
    return value


def public_job(snapshot: JobSnapshot[Any]) -> dict[str, Any]:
    return with_content_links(snapshot.to_public_dict())


__all__ = ["public_job", "with_content_links"]
