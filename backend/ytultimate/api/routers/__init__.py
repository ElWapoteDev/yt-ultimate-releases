"""Versioned API routers."""
"""Versioned API routers."""

from fastapi import APIRouter

from .developer import router as developer_router
from .discovery import router as discovery_router
from .downloads import router as downloads_router
from .library import router as library_router
from .playlists import router as playlists_router
from .processing import router as processing_router
from .settings import router as settings_router
from .streaming import router as streaming_router
from .system import router as system_router


def create_v1_router() -> APIRouter:
    router = APIRouter(prefix="/api/v1")
    for child in (
        system_router,
        settings_router,
        library_router,
        playlists_router,
        discovery_router,
        downloads_router,
        streaming_router,
        processing_router,
        developer_router,
    ):
        router.include_router(child)
    return router


__all__ = ["create_v1_router"]
