from __future__ import annotations

from typing import Any

from fastapi import APIRouter

from ...schemas import SettingsPatch
from ...services.settings import SettingsService
from ..dependencies import ContainerDependency
from ..models import SettingsResetRequest


router = APIRouter(prefix="/settings", tags=["settings"])


@router.get("")
def get_settings(container: ContainerDependency) -> dict[str, Any]:
    return container.require("settings", SettingsService).get_all()


@router.get("/schema")
def settings_schema(container: ContainerDependency) -> dict[str, Any]:
    service = container.require("settings", SettingsService)
    return {"settings": service.describe()}


async def _update_settings(
    payload: dict[str, Any],
    container: ContainerDependency,
) -> dict[str, Any]:
    service = container.require("settings", SettingsService)
    previous_developer_mode = bool(service.get("developer_mode"))
    updated = service.update(payload)
    next_developer_mode = bool(updated["developer_mode"])
    if next_developer_mode != previous_developer_mode:
        try:
            await container.set_developer_mode(next_developer_mode)
        except Exception:
            service.update({"developer_mode": previous_developer_mode})
            raise
    return updated


@router.patch("")
async def patch_settings(
    payload: SettingsPatch,
    container: ContainerDependency,
) -> dict[str, Any]:
    return await _update_settings(
        payload.model_dump(exclude_none=True),
        container,
    )


@router.post("")
async def post_settings(
    payload: SettingsPatch,
    container: ContainerDependency,
) -> dict[str, Any]:
    """Compatibility verb retained inside the versioned API."""

    return await _update_settings(
        payload.model_dump(exclude_none=True),
        container,
    )


@router.post("/reset")
async def reset_settings(
    payload: SettingsResetRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    service = container.require("settings", SettingsService)
    previous_developer_mode = bool(service.get("developer_mode"))
    updated = service.reset(payload.keys)
    next_developer_mode = bool(updated["developer_mode"])
    if next_developer_mode != previous_developer_mode:
        try:
            await container.set_developer_mode(next_developer_mode)
        except Exception:
            service.update({"developer_mode": previous_developer_mode})
            raise
    return updated


__all__ = ["router"]
