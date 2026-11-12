"""Central HTTP error mapping for service-layer exceptions."""

from __future__ import annotations

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse

from ..container import ContainerNotStartedError
from ..jobs import JobCapacityError, JobNotFoundError
from ..plugins import PluginError, PluginErrorCode
from ..services.downloads import DownloadEngineUnavailable, DownloadError
from ..services.library import (
    MediaInUseError,
    TrackNotFoundError,
    UnsafeLibraryPathError,
    UnsupportedMediaError,
)
from ..services.playlists import (
    PlaylistConflictError,
    PlaylistNotFoundError,
    TrackNotFoundError as PlaylistTrackNotFoundError,
)
from ..services.processing import ProcessingError
from ..services.search import (
    InvalidYouTubeURLError,
    SearchUnavailableError,
)
from ..services.streaming import StreamingError
from ..services.youtube import YoutubeFailure


def _response(status_code: int, detail: str, **extra: object) -> JSONResponse:
    return JSONResponse(
        status_code=status_code,
        content={"detail": detail, **extra},
    )


def install_exception_handlers(app: FastAPI) -> None:
    @app.exception_handler(YoutubeFailure)
    async def youtube_error(_request: Request, exc: YoutubeFailure) -> JSONResponse:
        status = {
            "configuration": 400,
            "unavailable": 404,
            "rate_limited": 429,
            "javascript_unavailable": 503,
        }.get(exc.code, 502)
        response = _response(status, exc.message, code=exc.code, retryable=exc.retryable)
        if exc.retry_after is not None:
            response.headers["Retry-After"] = str(exc.retry_after)
        return response

    @app.exception_handler(ContainerNotStartedError)
    async def container_not_ready(
        _request: Request,
        exc: ContainerNotStartedError,
    ) -> JSONResponse:
        return _response(503, str(exc))

    @app.exception_handler(FileNotFoundError)
    @app.exception_handler(TrackNotFoundError)
    @app.exception_handler(PlaylistNotFoundError)
    @app.exception_handler(PlaylistTrackNotFoundError)
    @app.exception_handler(JobNotFoundError)
    async def not_found(_request: Request, exc: Exception) -> JSONResponse:
        return _response(404, str(exc))

    @app.exception_handler(PlaylistConflictError)
    @app.exception_handler(MediaInUseError)
    async def conflict(
        _request: Request,
        exc: Exception,
    ) -> JSONResponse:
        return _response(409, str(exc))

    @app.exception_handler(PluginError)
    async def plugin_error(
        _request: Request,
        exc: PluginError,
    ) -> JSONResponse:
        if exc.code is PluginErrorCode.DEVELOPER_MODE_REQUIRED:
            status_code = 403
        elif exc.code in {
            PluginErrorCode.PLUGIN_NOT_FOUND,
            PluginErrorCode.COMMAND_NOT_FOUND,
        }:
            status_code = 404
        elif exc.code is PluginErrorCode.SCAFFOLD_EXISTS:
            status_code = 409
        elif exc.code in {
            PluginErrorCode.LOAD_TIMEOUT,
            PluginErrorCode.COMMAND_TIMEOUT,
        }:
            status_code = 504
        else:
            status_code = 400
        return _response(
            status_code,
            exc.message,
            error=exc.to_dict(),
        )

    for exception_type in (
        ValueError,
        TypeError,
        KeyError,
        InvalidYouTubeURLError,
        UnsafeLibraryPathError,
        UnsupportedMediaError,
    ):
        app.add_exception_handler(
            exception_type,
            lambda _request, exc: _response(400, str(exc)),
        )

    for exception_type in (
        DownloadEngineUnavailable,
        SearchUnavailableError,
    ):
        app.add_exception_handler(
            exception_type,
            lambda _request, exc: _response(503, str(exc)),
        )

    @app.exception_handler(DownloadError)
    async def download_error(_request: Request, exc: DownloadError) -> JSONResponse:
        if exc.failure is not None:
            return await youtube_error(_request, exc.failure)
        return _response(502, str(exc))

    for exception_type in (
        ProcessingError,
        StreamingError,
    ):
        app.add_exception_handler(
            exception_type,
            lambda _request, exc: _response(502, str(exc)),
        )

    @app.exception_handler(JobCapacityError)
    async def job_capacity(
        _request: Request,
        exc: JobCapacityError,
    ) -> JSONResponse:
        return _response(429, str(exc))


__all__ = ["install_exception_handlers"]
