"""FastAPI dependency accessors."""

from __future__ import annotations

from typing import Annotated

from fastapi import Depends, Request

from ..container import Container, ContainerNotStartedError


def get_container(request: Request) -> Container:
    container = getattr(request.app.state, "container", None)
    if not isinstance(container, Container) or not container.started:
        raise ContainerNotStartedError("Application services are not ready")
    return container


ContainerDependency = Annotated[Container, Depends(get_container)]


__all__ = ["ContainerDependency", "get_container"]
