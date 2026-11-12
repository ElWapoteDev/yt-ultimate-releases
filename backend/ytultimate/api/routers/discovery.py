from __future__ import annotations

import asyncio
from typing import Any

from fastapi import APIRouter, Query

from ...services.downloads import DownloadService
from ...services.search import SearchService
from ..dependencies import ContainerDependency


router = APIRouter(tags=["discovery"])


@router.get("/search")
async def search(
    container: ContainerDependency,
    q: str | None = None,
    query: str | None = None,
    limit: int = Query(default=20, ge=1, le=50),
) -> list[dict[str, Any]]:
    term = q if q is not None else query
    if term is None:
        raise ValueError("Search query is required")
    service = container.require("search", SearchService)
    return await asyncio.to_thread(service.search, term, limit)


@router.get("/playlist_info")
async def playlist_info(
    url: str,
    container: ContainerDependency,
) -> dict[str, Any]:
    service = container.require("downloads", DownloadService)
    return await asyncio.to_thread(service.playlist_info, url)


__all__ = ["router"]
