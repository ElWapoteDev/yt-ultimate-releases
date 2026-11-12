from __future__ import annotations

import asyncio
from typing import Any

from fastapi import APIRouter

from ...plugins import (
    PluginError,
    PluginErrorCode,
    PluginManager,
    create_plugin_scaffold,
    discover_plugin_examples,
    install_plugin_example,
)
from ...schemas import (
    PluginCommandRequest,
    PluginScaffoldRequest,
    PluginToggleRequest,
)
from ..dependencies import ContainerDependency
from ..models import PluginConfigRequest


router = APIRouter(prefix="/developer/plugins", tags=["developer"])


def _manager(container: ContainerDependency) -> PluginManager:
    manager = container.plugin_manager
    if manager is None:
        raise PluginError(
            PluginErrorCode.DEVELOPER_MODE_REQUIRED,
            "Developer mode must be enabled to use plugins",
        )
    return manager


def _public_snapshot(snapshot: Any) -> dict[str, Any]:
    item = snapshot.to_dict()
    runtime_status = item["status"]
    item["runtime_status"] = runtime_status
    item["loaded"] = runtime_status == "active"
    if item["loaded"]:
        item["status"] = "loaded"
    error = item.get("error")
    if isinstance(error, dict):
        item["diagnostic"] = error
        item["error"] = error.get("message", "Plugin error")
    return item


def _plugin_report(manager: PluginManager) -> dict[str, Any]:
    plugins = [_public_snapshot(item) for item in manager.snapshots()]
    return {
        "developer_mode": manager.developer_mode,
        "plugins": plugins,
        "count": len(plugins),
        "commands": [command.to_dict() for command in manager.commands()],
        "diagnostics": [
            diagnostic.to_dict() for diagnostic in manager.diagnostics
        ],
    }


@router.get("")
def list_plugins(
    container: ContainerDependency,
) -> dict[str, Any]:
    return _plugin_report(_manager(container))


@router.post("/reload")
async def reload_plugins(
    container: ContainerDependency,
) -> dict[str, Any]:
    manager = _manager(container)
    await manager.refresh()
    return _plugin_report(manager)


@router.post("/scaffold", status_code=201)
async def scaffold_plugin(
    payload: PluginScaffoldRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    manager = _manager(container)
    path = await asyncio.to_thread(
        create_plugin_scaffold,
        container.config.plugin_dir,
        payload.plugin_id,
        name=payload.name,
        description=payload.description,
    )
    await manager.refresh()
    return {
        "status": "created",
        "plugin_id": payload.plugin_id,
        "path": str(path),
    }


@router.get("/examples")
def list_plugin_examples(
    container: ContainerDependency,
) -> dict[str, Any]:
    manager = _manager(container)
    examples = discover_plugin_examples(container.config.plugin_examples_dir)
    installed = {
        item.id
        for item in manager.snapshots()
    }
    items = [
        example.to_dict(installed=example.manifest.id in installed)
        for example in examples
    ]
    return {"examples": items, "count": len(items)}


@router.post("/examples/{example_id}/install", status_code=201)
async def install_bundled_plugin_example(
    example_id: str,
    container: ContainerDependency,
) -> dict[str, Any]:
    manager = _manager(container)
    path = await asyncio.to_thread(
        install_plugin_example,
        container.config.plugin_examples_dir,
        container.config.plugin_dir,
        example_id,
    )
    await manager.refresh()
    return {
        "status": "installed",
        "plugin_id": example_id,
        "path": str(path),
        "plugin": _public_snapshot(manager.snapshot(example_id)),
    }


@router.post("/{plugin_id}/reload")
async def reload_plugin(
    plugin_id: str,
    container: ContainerDependency,
) -> dict[str, Any]:
    snapshot = await _manager(container).reload(plugin_id)
    return _public_snapshot(snapshot)


@router.put("/{plugin_id}/enabled")
async def set_plugin_enabled(
    plugin_id: str,
    payload: PluginToggleRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    manager = _manager(container)
    snapshot = (
        await manager.enable(plugin_id)
        if payload.enabled
        else await manager.disable(plugin_id)
    )
    return _public_snapshot(snapshot)


@router.get("/{plugin_id}/config")
def get_plugin_config(
    plugin_id: str,
    container: ContainerDependency,
) -> dict[str, Any]:
    manager = _manager(container)
    snapshot = manager.snapshot(plugin_id)
    return {
        "plugin_id": plugin_id,
        "config": manager.get_config(plugin_id),
        "schema": dict(snapshot.config_schema),
    }


@router.patch("/{plugin_id}/config")
async def configure_plugin(
    plugin_id: str,
    payload: PluginConfigRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    config = await _manager(container).configure(
        plugin_id,
        payload.config,
        replace=payload.replace,
    )
    return {"plugin_id": plugin_id, "config": config}


@router.post("/{plugin_id}/config/reset")
def reset_plugin_config(
    plugin_id: str,
    container: ContainerDependency,
) -> dict[str, Any]:
    config = _manager(container).reset_config(plugin_id)
    return {"plugin_id": plugin_id, "config": config}


@router.post("/{plugin_id}/commands/{command_name}")
async def run_plugin_command(
    plugin_id: str,
    command_name: str,
    payload: PluginCommandRequest,
    container: ContainerDependency,
) -> dict[str, Any]:
    manager = _manager(container)
    command_id = (
        command_name
        if command_name.startswith(f"{plugin_id}.")
        else f"{plugin_id}.{command_name}"
    )
    execution = await manager.run_command(command_id, payload.arguments)
    if not execution.ok and execution.error is not None:
        raise execution.error
    return {
        "ok": True,
        "plugin_id": plugin_id,
        "command": command_name,
        "result": execution.value,
        "message": "Comando ejecutado",
        "duration_ms": round(execution.duration_ms, 3),
    }


__all__ = ["router"]
