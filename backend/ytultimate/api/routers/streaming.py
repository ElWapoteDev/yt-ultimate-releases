from __future__ import annotations

import asyncio
from typing import Any
from urllib.parse import quote

from fastapi import APIRouter, Request, Response

from ...services.streaming import StreamingService
from ..dependencies import ContainerDependency


router = APIRouter(prefix="/stream", tags=["streaming"])


@router.get("")
async def resolve_stream(
    url: str,
    container: ContainerDependency,
    refresh: bool = False,
) -> dict[str, Any]:
    service = container.require("streaming", StreamingService)
    info = await asyncio.to_thread(service.get_stream, url, refresh=refresh)
    info.pop("audio_url", None)
    info["proxy_path"] = (
        f"/api/v1/stream/proxy?url={quote(url, safe='')}"
    )
    return info


def _proxy(
    request: Request,
    url: str,
    container: ContainerDependency,
    *,
    refresh: bool,
) -> Response:
    return container.require(
        "streaming",
        StreamingService,
    ).build_proxy_response(
        url,
        request.headers,
        method=request.method,
        refresh=refresh,
    )


@router.get("/proxy")
def proxy_stream(
    request: Request,
    url: str,
    container: ContainerDependency,
    refresh: bool = False,
) -> Response:
    return _proxy(request, url, container, refresh=refresh)


@router.head("/proxy")
def proxy_stream_head(
    request: Request,
    url: str,
    container: ContainerDependency,
    refresh: bool = False,
) -> Response:
    return _proxy(request, url, container, refresh=refresh)


__all__ = ["router"]
