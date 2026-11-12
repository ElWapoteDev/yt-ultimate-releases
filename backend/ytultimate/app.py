"""FastAPI application factory for the local desktop service."""

from __future__ import annotations

import ipaddress
import os
import secrets
import sys
import threading
import time
from contextlib import asynccontextmanager
from typing import Any, AsyncIterator

import uvicorn
from fastapi import FastAPI, Request
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse
from starlette.middleware.base import BaseHTTPMiddleware, RequestResponseEndpoint
from starlette.middleware.trustedhost import TrustedHostMiddleware
from starlette.responses import Response

from . import __version__
from .api import create_v1_router
from .api.errors import install_exception_handlers
from .config import AppConfig
from .container import Container


API_PREFIX = "/api/v1"
TOKEN_HEADER = "X-YTU-Token"
STDIN_SHUTDOWN_ENV = "YTU_EXIT_ON_STDIN_CLOSE"
STREAM_SHUTDOWN_TIMEOUT_SECONDS = 2
SHUTDOWN_GRACE_SECONDS = 10.0


class LocalTokenMiddleware(BaseHTTPMiddleware):
    """Authenticate API calls with the per-launch desktop token."""

    def __init__(self, app: FastAPI, *, token: str) -> None:
        super().__init__(app)
        self._token = token

    async def dispatch(
        self,
        request: Request,
        call_next: RequestResponseEndpoint,
    ) -> Response:
        if (
            self._token
            and request.method != "OPTIONS"
            and request.url.path.startswith(f"{API_PREFIX}/")
        ):
            supplied = (
                request.headers.get(TOKEN_HEADER)
                or request.query_params.get("token")
                or ""
            )
            if not secrets.compare_digest(supplied, self._token):
                return JSONResponse(
                    status_code=401,
                    content={"detail": "Invalid or missing local API token"},
                    headers={"WWW-Authenticate": "YTU-Token"},
                )
        response = await call_next(request)
        response.headers.setdefault("X-Content-Type-Options", "nosniff")
        response.headers.setdefault("Referrer-Policy", "no-referrer")
        return response


def create_app(
    config: AppConfig | None = None,
    *,
    container: Container | None = None,
) -> FastAPI:
    runtime_config = config or (container.config if container else AppConfig.from_env())
    runtime_container = container or Container(runtime_config)

    @asynccontextmanager
    async def lifespan(application: FastAPI) -> AsyncIterator[None]:
        await runtime_container.start()
        application.state.container = runtime_container
        try:
            yield
        finally:
            await runtime_container.shutdown()

    application = FastAPI(
        title="YT Ultimate Local API",
        version=__version__,
        description=(
            "Versioned loopback API for the YT Ultimate macOS application."
        ),
        lifespan=lifespan,
        docs_url=None,
        redoc_url=None,
        openapi_url=f"{API_PREFIX}/openapi.json",
    )
    application.state.container = runtime_container
    application.include_router(create_v1_router())
    install_exception_handlers(application)

    application.add_middleware(
        TrustedHostMiddleware,
        allowed_hosts=["127.0.0.1", "localhost", "[::1]", "testserver"],
    )
    origins = tuple(
        origin
        for origin in runtime_config.allowed_origins
        if origin and origin != "*"
    )
    if origins:
        application.add_middleware(
            CORSMiddleware,
            allow_origins=list(origins),
            allow_credentials=True,
            allow_methods=["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"],
            allow_headers=[
                "Accept",
                "Content-Type",
                "If-Range",
                "Range",
                TOKEN_HEADER,
            ],
            expose_headers=[
                "Accept-Ranges",
                "Content-Length",
                "Content-Range",
                "ETag",
                "Last-Modified",
            ],
            max_age=600,
        )
    application.add_middleware(
        LocalTokenMiddleware,
        token=runtime_config.auth_token,
    )
    return application


def _require_loopback(host: str) -> None:
    normalized = host.strip().strip("[]").lower()
    if normalized == "localhost":
        return
    try:
        address = ipaddress.ip_address(normalized)
    except ValueError as exc:
        raise ValueError("YT Ultimate only binds to a loopback address") from exc
    if not address.is_loopback:
        raise ValueError("YT Ultimate only binds to a loopback address")


def _shutdown_when_stdin_closes(server: uvicorn.Server) -> None:
    """Stop gracefully once the desktop shell closes stdin or exits.

    Windows cannot deliver SIGTERM to another process, so Electron requests
    shutdown by closing the pipe. A crashed shell closes it as well, which
    keeps the service from outliving the application.
    """

    def watch() -> None:
        try:
            descriptor = sys.stdin.fileno()
            while os.read(descriptor, 4096):
                pass
        except (AttributeError, OSError, ValueError):
            pass
        server.should_exit = True
        time.sleep(SHUTDOWN_GRACE_SECONDS)
        os._exit(0)

    threading.Thread(target=watch, name="ytu-stdin-shutdown", daemon=True).start()


def run(config: AppConfig | None = None) -> None:
    runtime_config = config or AppConfig.from_env()
    _require_loopback(runtime_config.host)
    application = create_app(runtime_config)
    options: dict[str, Any] = {
        "host": runtime_config.host,
        "port": runtime_config.port,
        "log_level": "info",
        "access_log": False,
        "server_header": False,
    }
    if os.getenv(STDIN_SHUTDOWN_ENV) != "1":
        uvicorn.run(application, **options)
        return
    # Media streams can stay open while the player exists; they must not delay
    # the lifespan shutdown that cancels jobs and their subprocesses.
    server = uvicorn.Server(
        uvicorn.Config(
            application,
            timeout_graceful_shutdown=STREAM_SHUTDOWN_TIMEOUT_SECONDS,
            **options,
        )
    )
    _shutdown_when_stdin_closes(server)
    try:
        server.run()
    except KeyboardInterrupt:
        pass
    if not server.started:
        # Mirror uvicorn.run(), which reports a failed startup with status 3.
        raise SystemExit(3)


app = create_app()


__all__ = ["API_PREFIX", "TOKEN_HEADER", "app", "create_app", "run"]
