"""Opt-in lifecycle manager for trusted in-process developer plugins."""

from __future__ import annotations

import asyncio
import hashlib
import importlib.util
import inspect
import itertools
import logging
import math
import sys
import threading
import time
import types
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from typing import Any, Callable, Mapping

from ytultimate.events import EventBus, Subscription

from .errors import PluginError, PluginErrorCode
from .manifest import (
    MANIFEST_FILENAME,
    PluginManifest,
    merge_plugin_config,
    validate_plugin_id,
)
from .sdk import (
    MAX_HANDLER_TIMEOUT,
    MIN_HANDLER_TIMEOUT,
    CommandDescriptor,
    CommandExecutionResult,
    PluginAPI,
    PluginPaths,
    _CommandRegistration,
    _RegistrationBuilder,
)
from .state import JsonStateStore, PluginState, StateStore
from .validation import ValidationIssue, ensure_json_value


class PluginStatus(str, Enum):
    DISABLED = "disabled"
    ENABLED = "enabled"
    ACTIVE = "active"
    BLOCKED = "blocked"
    ERROR = "error"


@dataclass(frozen=True, slots=True)
class PluginDiagnostic:
    """A folder that could not become a discoverable plugin."""

    directory: str
    error: PluginError

    def to_dict(self) -> dict[str, Any]:
        return {"directory": self.directory, "error": self.error.to_dict()}


@dataclass(frozen=True, slots=True)
class PluginSnapshot:
    """Serializable plugin state for developer settings and diagnostics."""

    id: str
    name: str
    version: str
    description: str
    author: str
    directory: str
    enabled: bool
    status: PluginStatus
    permissions: tuple[str, ...]
    config: Mapping[str, Any]
    config_schema: Mapping[str, Any]
    commands: tuple[CommandDescriptor, ...]
    error: PluginError | None = None

    def to_dict(self) -> dict[str, Any]:
        result: dict[str, Any] = {
            "id": self.id,
            "name": self.name,
            "version": self.version,
            "description": self.description,
            "author": self.author,
            "directory": self.directory,
            "enabled": self.enabled,
            "status": self.status.value,
            "permissions": list(self.permissions),
            "config": _clone_json(self.config),
            "config_schema": _clone_json(self.config_schema),
            "commands": [command.to_dict() for command in self.commands],
        }
        if self.error is not None:
            result["error"] = self.error.to_dict()
        return result


@dataclass(slots=True)
class _PluginRecord:
    manifest: PluginManifest
    root: Path
    state: PluginState
    config: dict[str, Any]
    status: PluginStatus
    last_error: PluginError | None = None


@dataclass(slots=True)
class _LoadedPlugin:
    module_prefix: str
    api: PluginAPI
    commands: dict[str, _CommandRegistration]
    subscriptions: list[Subscription]
    teardowns: list[Callable[..., Any]]


class PluginManager:
    """Discovers and runs explicitly enabled plugins only in developer mode.

    Python plugins execute in-process and therefore must be trusted. Manifest
    permissions are declarations for host-provided APIs and auditing; they cannot
    sandbox arbitrary Python imports or filesystem access.
    """

    def __init__(
        self,
        plugin_dir: Path | str,
        *,
        developer_mode: bool = False,
        state_store: StateStore | None = None,
        event_bus: EventBus | None = None,
        data_dir: Path | str | None = None,
        load_timeout: float = 10.0,
        command_timeout: float = 30.0,
        teardown_timeout: float = 5.0,
        logger: logging.Logger | None = None,
    ) -> None:
        if not isinstance(developer_mode, bool):
            raise TypeError("developer_mode must be a boolean")
        for label, value in (
            ("load_timeout", load_timeout),
            ("command_timeout", command_timeout),
            ("teardown_timeout", teardown_timeout),
        ):
            _validate_manager_timeout(label, value)

        self.plugin_dir = Path(plugin_dir).expanduser()
        self.data_dir = (
            Path(data_dir).expanduser()
            if data_dir is not None
            else self.plugin_dir / ".data"
        )
        self._developer_mode = developer_mode
        self.state_store: StateStore = (
            state_store
            if state_store is not None
            else JsonStateStore(self.plugin_dir / ".plugin-state-v1.json")
        )
        if not callable(getattr(self.state_store, "load", None)) or not callable(
            getattr(self.state_store, "save", None)
        ):
            raise TypeError("state_store must implement load(plugin_id) and save(...)")
        self.event_bus = event_bus if event_bus is not None else EventBus()
        self.load_timeout = float(load_timeout)
        self.command_timeout = float(command_timeout)
        self.teardown_timeout = float(teardown_timeout)
        self._logger = logger or logging.getLogger("ytultimate.plugins")
        self._records: dict[str, _PluginRecord] = {}
        self._loaded: dict[str, _LoadedPlugin] = {}
        self._commands: dict[str, _CommandRegistration] = {}
        self._diagnostics: tuple[PluginDiagnostic, ...] = ()
        self._state_lock = threading.RLock()
        self._lifecycle_lock = asyncio.Lock()
        self._generation = itertools.count(1)
        self._started = False

    @property
    def developer_mode(self) -> bool:
        return self._developer_mode

    @property
    def started(self) -> bool:
        return self._started

    @property
    def diagnostics(self) -> tuple[PluginDiagnostic, ...]:
        with self._state_lock:
            return self._diagnostics

    def discover(self) -> tuple[PluginSnapshot, ...]:
        """Read/validate plugin folders without importing any plugin code."""

        self.plugin_dir.mkdir(parents=True, exist_ok=True)
        resolved_plugin_dir = self.plugin_dir.resolve()
        discovered: dict[str, _PluginRecord] = {}
        diagnostics: list[PluginDiagnostic] = []

        try:
            candidates = sorted(
                (
                    path
                    for path in self.plugin_dir.iterdir()
                    if not path.name.startswith(".")
                    and (path.is_dir() or path.is_symlink())
                ),
                key=lambda path: path.name,
            )
        except OSError as exc:
            raise PluginError(
                PluginErrorCode.LOAD_FAILED,
                "plugin directory cannot be read",
                cause=exc,
            ) from exc

        for candidate in candidates:
            if candidate.is_symlink():
                diagnostics.append(
                    PluginDiagnostic(
                        directory=candidate.name,
                        error=PluginError(
                            PluginErrorCode.PATH_ESCAPE,
                            "symlink plugin directories are not allowed",
                        ),
                    )
                )
                continue
            try:
                resolved_candidate = candidate.resolve(strict=True)
                resolved_candidate.relative_to(resolved_plugin_dir)
            except (OSError, ValueError) as exc:
                diagnostics.append(
                    PluginDiagnostic(
                        directory=candidate.name,
                        error=PluginError(
                            PluginErrorCode.PATH_ESCAPE,
                            "plugin directory must stay inside the configured root",
                            cause=exc,
                        ),
                    )
                )
                continue

            manifest_path = candidate / MANIFEST_FILENAME
            if not manifest_path.is_file():
                continue
            try:
                if manifest_path.is_symlink():
                    raise PluginError(
                        PluginErrorCode.PATH_ESCAPE,
                        "symlink plugin manifests are not allowed",
                    )
                manifest = PluginManifest.from_file(manifest_path)
                if candidate.name != manifest.id:
                    raise PluginError(
                        PluginErrorCode.INVALID_PLUGIN_ID,
                        "plugin folder name must exactly match its manifest ID",
                        plugin_id=manifest.id,
                        details={
                            "folder": candidate.name,
                            "expected": manifest.id,
                        },
                    )
                if manifest.id in discovered:
                    raise PluginError(
                        PluginErrorCode.DUPLICATE_PLUGIN_ID,
                        "duplicate plugin ID",
                        plugin_id=manifest.id,
                    )
                manifest.resolve_entrypoint(candidate)
                state = self._load_state(manifest.id)
                config = merge_plugin_config(manifest, state.config)
                previous = self._records.get(manifest.id)
                if manifest.id in self._loaded and previous is not None:
                    status = PluginStatus.ACTIVE
                    last_error = previous.last_error
                elif state.enabled and not self._developer_mode:
                    status = PluginStatus.BLOCKED
                    last_error = None
                elif state.enabled:
                    status = PluginStatus.ENABLED
                    last_error = None
                else:
                    status = PluginStatus.DISABLED
                    last_error = None
                discovered[manifest.id] = _PluginRecord(
                    manifest=manifest,
                    root=resolved_candidate,
                    state=state,
                    config=config,
                    status=status,
                    last_error=last_error,
                )
            except PluginError as exc:
                diagnostics.append(
                    PluginDiagnostic(directory=candidate.name, error=exc)
                )

        with self._state_lock:
            self._records = discovered
            self._diagnostics = tuple(diagnostics)
        return self.snapshots()

    async def start(self) -> tuple[PluginSnapshot, ...]:
        """Discover and activate enabled plugins when developer mode permits."""

        async with self._lifecycle_lock:
            if self._started:
                return self.snapshots()
            self.discover()
            self._started = True
            if self._developer_mode:
                for plugin_id in sorted(self._records):
                    record = self._records[plugin_id]
                    if record.state.enabled and plugin_id not in self._loaded:
                        try:
                            await self._load(record)
                        except PluginError:
                            # Each plugin is isolated; diagnostics remain in its
                            # snapshot and other enabled plugins still start.
                            continue
        return self.snapshots()

    async def stop(self) -> None:
        """Unload all code without changing persisted opt-in choices."""

        async with self._lifecycle_lock:
            for plugin_id in tuple(self._loaded):
                try:
                    await self._unload(plugin_id)
                except PluginError:
                    self._logger.exception("Plugin %s failed during shutdown", plugin_id)
            self._started = False

    async def refresh(self) -> tuple[PluginSnapshot, ...]:
        """Cleanly rescan disk and reload all enabled plugins."""

        async with self._lifecycle_lock:
            for plugin_id in tuple(self._loaded):
                try:
                    await self._unload(plugin_id)
                except PluginError:
                    self._logger.exception("Plugin %s failed during refresh", plugin_id)
            self.discover()
            if self._developer_mode:
                for plugin_id in sorted(self._records):
                    record = self._records[plugin_id]
                    if record.state.enabled:
                        try:
                            await self._load(record)
                        except PluginError:
                            continue
        return self.snapshots()

    async def set_developer_mode(self, enabled: bool) -> tuple[PluginSnapshot, ...]:
        """Apply a host preference change without mutating plugin opt-in state."""

        if not isinstance(enabled, bool):
            raise TypeError("enabled must be a boolean")
        async with self._lifecycle_lock:
            if enabled == self._developer_mode:
                return self.snapshots()
            self._developer_mode = enabled
            if not enabled:
                for plugin_id in tuple(self._loaded):
                    try:
                        await self._unload(plugin_id)
                    except PluginError:
                        self._logger.exception(
                            "Plugin %s failed while disabling developer mode",
                            plugin_id,
                        )
                with self._state_lock:
                    for record in self._records.values():
                        record.status = (
                            PluginStatus.BLOCKED
                            if record.state.enabled
                            else PluginStatus.DISABLED
                        )
            else:
                for record in tuple(self._records.values()):
                    if record.state.enabled:
                        try:
                            await self._load(record)
                        except PluginError:
                            continue
        return self.snapshots()

    async def enable(self, plugin_id: str) -> PluginSnapshot:
        """Persist opt-in and load one plugin. Developer mode is mandatory."""

        async with self._lifecycle_lock:
            record = self._get_record(plugin_id)
            if not self._developer_mode:
                raise PluginError(
                    PluginErrorCode.DEVELOPER_MODE_REQUIRED,
                    "developer mode must be enabled before loading plugins",
                    plugin_id=plugin_id,
                )
            state = PluginState(
                enabled=True,
                config=_clone_json(record.state.config),
                updated_at=time.time(),
            )
            self._save_state(plugin_id, state)
            with self._state_lock:
                record.state = state
                record.status = PluginStatus.ENABLED
                record.last_error = None
            if plugin_id not in self._loaded:
                await self._load(record)
        return self.snapshot(plugin_id)

    async def disable(self, plugin_id: str) -> PluginSnapshot:
        """Unload one plugin and persist disabled state, even if teardown fails."""

        async with self._lifecycle_lock:
            record = self._get_record(plugin_id)
            # Persist opt-out first. Even if teardown or process shutdown fails,
            # the plugin cannot be imported again on the next launch.
            state = PluginState(
                enabled=False,
                config=_clone_json(record.state.config),
                updated_at=time.time(),
            )
            self._save_state(plugin_id, state)
            with self._state_lock:
                record.state = state
            teardown_error: PluginError | None = None
            if plugin_id in self._loaded:
                try:
                    await self._unload(plugin_id)
                except PluginError as exc:
                    teardown_error = exc
            with self._state_lock:
                record.status = PluginStatus.DISABLED
                record.last_error = teardown_error
        await self.event_bus.emit(
            "plugin.disabled",
            {"plugin_id": plugin_id},
            source="plugin-manager",
        )
        return self.snapshot(plugin_id)

    async def reload(self, plugin_id: str) -> PluginSnapshot:
        """Unload and freshly import an enabled plugin from disk."""

        async with self._lifecycle_lock:
            record = self._get_record(plugin_id)
            if not self._developer_mode:
                raise PluginError(
                    PluginErrorCode.DEVELOPER_MODE_REQUIRED,
                    "developer mode must be enabled before reloading plugins",
                    plugin_id=plugin_id,
                )
            if not record.state.enabled:
                raise PluginError(
                    PluginErrorCode.PLUGIN_DISABLED,
                    "plugin must be enabled before it can be reloaded",
                    plugin_id=plugin_id,
                )
            if plugin_id in self._loaded:
                try:
                    await self._unload(plugin_id)
                except PluginError:
                    # Teardown failure does not leave registrations live and does
                    # not prevent a clean import of the new generation.
                    self._logger.exception(
                        "Plugin %s teardown failed before reload",
                        plugin_id,
                    )
            # Re-read only after cleanup so code/config/manifest changes apply.
            self.discover()
            record = self._get_record(plugin_id)
            await self._load(record)
        return self.snapshot(plugin_id)

    def get_config(self, plugin_id: str) -> dict[str, Any]:
        with self._state_lock:
            record = self._get_record(plugin_id)
            return _clone_json(record.config)

    def set_config(
        self,
        plugin_id: str,
        updates: Mapping[str, Any],
        *,
        replace: bool = False,
    ) -> dict[str, Any]:
        """Validate and atomically persist config; loaded APIs see it immediately."""

        if not isinstance(updates, Mapping):
            raise PluginError(
                PluginErrorCode.CONFIG_INVALID,
                "plugin configuration updates must be an object",
                plugin_id=plugin_id,
            )
        if not isinstance(replace, bool):
            raise TypeError("replace must be a boolean")
        with self._state_lock:
            record = self._get_record(plugin_id)
            overrides = (
                _clone_json(updates)
                if replace
                else _deep_merge(
                    _clone_json(record.state.config),
                    _clone_json(updates),
                )
            )
            config = merge_plugin_config(record.manifest, overrides)
            state = PluginState(
                enabled=record.state.enabled,
                config=overrides,
                updated_at=time.time(),
            )
            # Save before publishing the in-memory state.
            self._save_state(plugin_id, state)
            record.state = state
            record.config = config
            return _clone_json(config)

    async def configure(
        self,
        plugin_id: str,
        updates: Mapping[str, Any],
        *,
        replace: bool = False,
    ) -> dict[str, Any]:
        """Async config update that also emits ``plugin.config_changed``."""

        config = self.set_config(plugin_id, updates, replace=replace)
        await self.event_bus.emit(
            "plugin.config_changed",
            {"plugin_id": plugin_id, "config": _clone_json(config)},
            source="plugin-manager",
        )
        return config

    def reset_config(self, plugin_id: str) -> dict[str, Any]:
        return self.set_config(plugin_id, {}, replace=True)

    def commands(self) -> tuple[CommandDescriptor, ...]:
        with self._state_lock:
            return tuple(
                registration.descriptor
                for _, registration in sorted(self._commands.items())
            )

    async def execute_command(
        self,
        command_id: str,
        payload: Mapping[str, Any] | None = None,
        *,
        timeout: float | None = None,
    ) -> Any:
        """Execute an active namespaced command or raise ``PluginError``."""

        if payload is not None and not isinstance(payload, Mapping):
            raise TypeError("command payload must be a mapping or None")
        if not isinstance(command_id, str) or not command_id:
            raise TypeError("command_id must be a non-empty string")
        if timeout is not None:
            _validate_manager_timeout("timeout", timeout)
        with self._state_lock:
            registration = self._commands.get(command_id)
        if registration is None:
            raise PluginError(
                PluginErrorCode.COMMAND_NOT_FOUND,
                f"command {command_id!r} is not registered",
                details={"command_id": command_id},
            )
        effective_timeout = (
            float(timeout)
            if timeout is not None
            else registration.descriptor.timeout
            if registration.descriptor.timeout is not None
            else self.command_timeout
        )
        try:
            value = await _invoke_callable(
                registration.handler,
                dict(payload or {}),
                timeout=effective_timeout,
            )
            ensure_json_value(value, "$.result")
            return value
        except TimeoutError as exc:
            raise PluginError(
                PluginErrorCode.COMMAND_TIMEOUT,
                f"command exceeded {effective_timeout:g} seconds",
                plugin_id=registration.descriptor.plugin_id,
                details={"command_id": command_id},
                cause=exc,
            ) from exc
        except PluginError:
            raise
        except ValidationIssue as exc:
            raise PluginError(
                PluginErrorCode.COMMAND_FAILED,
                "command returned a value that is not JSON-compatible",
                plugin_id=registration.descriptor.plugin_id,
                details={"command_id": command_id, "reason": str(exc)},
                cause=exc,
            ) from exc
        except Exception as exc:
            self._logger.exception("Plugin command %s failed", command_id)
            raise PluginError(
                PluginErrorCode.COMMAND_FAILED,
                str(exc) or "plugin command failed",
                plugin_id=registration.descriptor.plugin_id,
                details={
                    "command_id": command_id,
                    "exception_type": exc.__class__.__name__,
                },
                cause=exc,
            ) from exc

    async def run_command(
        self,
        command_id: str,
        payload: Mapping[str, Any] | None = None,
        *,
        timeout: float | None = None,
    ) -> CommandExecutionResult:
        """Never-throw wrapper for transport layers and developer consoles."""

        started_at = time.monotonic()
        try:
            value = await self.execute_command(command_id, payload, timeout=timeout)
            return CommandExecutionResult(
                command_id=command_id,
                ok=True,
                value=value,
                duration_ms=(time.monotonic() - started_at) * 1_000,
            )
        except PluginError as exc:
            return CommandExecutionResult(
                command_id=command_id,
                ok=False,
                error=exc,
                duration_ms=(time.monotonic() - started_at) * 1_000,
            )

    def snapshot(self, plugin_id: str) -> PluginSnapshot:
        with self._state_lock:
            record = self._get_record(plugin_id)
            commands = tuple(
                registration.descriptor
                for registration in self._commands.values()
                if registration.descriptor.plugin_id == plugin_id
            )
            commands = tuple(sorted(commands, key=lambda item: item.id))
            return PluginSnapshot(
                id=record.manifest.id,
                name=record.manifest.name,
                version=record.manifest.version,
                description=record.manifest.description,
                author=record.manifest.author,
                directory=record.root.name,
                enabled=record.state.enabled,
                status=record.status,
                permissions=record.manifest.permissions,
                config=_clone_json(record.config),
                config_schema=record.manifest.to_dict()["config_schema"],
                commands=commands,
                error=record.last_error,
            )

    def snapshots(self) -> tuple[PluginSnapshot, ...]:
        with self._state_lock:
            plugin_ids = sorted(self._records)
        return tuple(self.snapshot(plugin_id) for plugin_id in plugin_ids)

    async def _load(self, record: _PluginRecord) -> None:
        plugin_id = record.manifest.id
        if not self._developer_mode:
            raise PluginError(
                PluginErrorCode.DEVELOPER_MODE_REQUIRED,
                "developer mode is required to import plugin code",
                plugin_id=plugin_id,
            )
        if not record.state.enabled:
            raise PluginError(
                PluginErrorCode.PLUGIN_DISABLED,
                "plugin is not enabled",
                plugin_id=plugin_id,
            )
        if plugin_id in self._loaded:
            raise PluginError(
                PluginErrorCode.PLUGIN_ALREADY_LOADED,
                "plugin is already loaded",
                plugin_id=plugin_id,
            )

        entrypoint_path, callable_name = record.manifest.resolve_entrypoint(record.root)
        module_prefix = self._module_prefix(record)
        builder = _RegistrationBuilder(plugin_id)
        data_path = self.data_dir / plugin_id
        try:
            data_path.mkdir(parents=True, exist_ok=True)
        except OSError as exc:
            error = PluginError(
                PluginErrorCode.LOAD_FAILED,
                "plugin data directory could not be created",
                plugin_id=plugin_id,
                cause=exc,
            )
            self._set_error(record, error)
            raise error from exc
        logger = logging.LoggerAdapter(self._logger, {"plugin_id": plugin_id})
        api = PluginAPI(
            manifest=record.manifest,
            paths=PluginPaths(root=record.root, data=data_path),
            event_bus=self.event_bus,
            config_getter=lambda: self.get_config(plugin_id),
            registrations=builder,
            logger=logger,
        )

        try:
            import_aborted = threading.Event()

            def import_entrypoint() -> types.ModuleType:
                try:
                    return self._import_module(
                        module_prefix,
                        record.root,
                        entrypoint_path,
                    )
                finally:
                    if import_aborted.is_set():
                        self._remove_modules(module_prefix)

            try:
                module = await _invoke_callable(
                    import_entrypoint,
                    timeout=self.load_timeout,
                )
            except TimeoutError as exc:
                import_aborted.set()
                raise PluginError(
                    PluginErrorCode.LOAD_TIMEOUT,
                    f"plugin import exceeded {self.load_timeout:g} seconds",
                    plugin_id=plugin_id,
                    cause=exc,
                ) from exc
            except PluginError:
                raise
            except Exception as exc:
                raise PluginError(
                    PluginErrorCode.LOAD_FAILED,
                    str(exc) or "plugin module import failed",
                    plugin_id=plugin_id,
                    details={"exception_type": exc.__class__.__name__},
                    cause=exc,
                ) from exc

            entrypoint = getattr(module, callable_name, None)
            if not callable(entrypoint):
                raise PluginError(
                    PluginErrorCode.INVALID_ENTRYPOINT,
                    f"entrypoint callable {callable_name!r} was not found",
                    plugin_id=plugin_id,
                )
            try:
                teardown = await _invoke_callable(
                    entrypoint,
                    api,
                    timeout=self.load_timeout,
                )
            except TimeoutError as exc:
                raise PluginError(
                    PluginErrorCode.LOAD_TIMEOUT,
                    f"plugin setup exceeded {self.load_timeout:g} seconds",
                    plugin_id=plugin_id,
                    cause=exc,
                ) from exc
            except PluginError:
                raise
            except Exception as exc:
                raise PluginError(
                    PluginErrorCode.SETUP_FAILED,
                    str(exc) or "plugin setup failed",
                    plugin_id=plugin_id,
                    details={"exception_type": exc.__class__.__name__},
                    cause=exc,
                ) from exc
            if teardown is not None:
                if not callable(teardown):
                    raise PluginError(
                        PluginErrorCode.SETUP_FAILED,
                        "setup must return None or a teardown callable",
                        plugin_id=plugin_id,
                    )
                builder.add_teardown(teardown)
            builder.seal()
            try:
                loaded = self._commit_registrations(
                    plugin_id,
                    module_prefix,
                    api,
                    builder,
                )
            except PluginError:
                raise
            except Exception as exc:
                raise PluginError(
                    PluginErrorCode.SETUP_FAILED,
                    "plugin registrations could not be committed",
                    plugin_id=plugin_id,
                    details={"exception_type": exc.__class__.__name__},
                    cause=exc,
                ) from exc
            with self._state_lock:
                self._loaded[plugin_id] = loaded
                record.status = PluginStatus.ACTIVE
                record.last_error = None
        except PluginError as error:
            builder.seal()
            self._remove_modules(module_prefix)
            self._set_error(record, error)
            self._logger.error(
                "Plugin %s failed to load: %s",
                plugin_id,
                error.message,
                exc_info=error.cause,
            )
            raise

        self.event_bus.emit_nowait(
            "plugin.loaded",
            {
                "plugin_id": plugin_id,
                "version": record.manifest.version,
            },
            source="plugin-manager",
        )

    def _commit_registrations(
        self,
        plugin_id: str,
        module_prefix: str,
        api: PluginAPI,
        builder: _RegistrationBuilder,
    ) -> _LoadedPlugin:
        command_map = {
            registration.descriptor.id: registration
            for registration in builder.commands
        }
        subscriptions: list[Subscription] = []
        with self._state_lock:
            conflicts = set(command_map).intersection(self._commands)
            if conflicts:
                raise PluginError(
                    PluginErrorCode.COMMAND_CONFLICT,
                    "one or more command IDs are already registered",
                    plugin_id=plugin_id,
                    details={"commands": sorted(conflicts)},
                )
            try:
                for hook in builder.hooks:
                    subscriptions.append(
                        self.event_bus.subscribe(
                            hook.event_name,
                            hook.handler,
                            owner=plugin_id,
                            priority=hook.priority,
                            once=hook.once,
                            timeout=hook.timeout,
                        )
                    )
                self._commands.update(command_map)
            except Exception:
                for subscription in subscriptions:
                    subscription.unsubscribe()
                for command_id in command_map:
                    self._commands.pop(command_id, None)
                raise
        return _LoadedPlugin(
            module_prefix=module_prefix,
            api=api,
            commands=command_map,
            subscriptions=subscriptions,
            teardowns=list(builder.teardowns),
        )

    async def _unload(self, plugin_id: str) -> None:
        with self._state_lock:
            loaded = self._loaded.pop(plugin_id, None)
            record = self._records.get(plugin_id)
            if loaded is None:
                return
            for command_id in loaded.commands:
                self._commands.pop(command_id, None)
            for subscription in loaded.subscriptions:
                subscription.unsubscribe()

        failures: list[dict[str, str]] = []
        for callback in reversed(loaded.teardowns):
            try:
                await _invoke_callable(
                    callback,
                    timeout=self.teardown_timeout,
                )
            except Exception as exc:  # noqa: BLE001 - cleanup must continue.
                failures.append(
                    {
                        "callback": getattr(
                            callback,
                            "__qualname__",
                            callback.__class__.__name__,
                        ),
                        "error": str(exc) or exc.__class__.__name__,
                        "type": exc.__class__.__name__,
                    }
                )
                self._logger.exception(
                    "Plugin %s teardown callback failed",
                    plugin_id,
                )
        self._remove_modules(loaded.module_prefix)
        if record is not None:
            with self._state_lock:
                record.status = (
                    PluginStatus.ENABLED
                    if record.state.enabled and self._developer_mode
                    else PluginStatus.BLOCKED
                    if record.state.enabled
                    else PluginStatus.DISABLED
                )
        if failures:
            error = PluginError(
                PluginErrorCode.TEARDOWN_FAILED,
                "one or more plugin teardown callbacks failed",
                plugin_id=plugin_id,
                details={"failures": failures},
            )
            if record is not None:
                record.last_error = error
            raise error

    @staticmethod
    def _import_module(
        module_prefix: str,
        plugin_root: Path,
        entrypoint_path: Path,
    ) -> types.ModuleType:
        relative = entrypoint_path.relative_to(plugin_root)
        module_parts = list(relative.with_suffix("").parts)
        _install_namespace(module_prefix, plugin_root)
        for index in range(1, len(module_parts)):
            package_name = f"{module_prefix}.{'.'.join(module_parts[:index])}"
            package_path = plugin_root.joinpath(*module_parts[:index])
            _install_namespace(package_name, package_path)
        module_name = f"{module_prefix}.{'.'.join(module_parts)}"
        spec = importlib.util.spec_from_file_location(module_name, entrypoint_path)
        if spec is None or spec.loader is None:
            raise ImportError("could not create an import spec for the entrypoint")
        module = importlib.util.module_from_spec(spec)
        sys.modules[module_name] = module
        try:
            # Compile the entrypoint source directly so developer-mode reloads
            # never reuse a same-second stale bytecode cache.
            source = entrypoint_path.read_bytes()
            code = compile(
                source,
                str(entrypoint_path),
                "exec",
                dont_inherit=True,
            )
            exec(code, module.__dict__)
        except BaseException:
            for name in tuple(sys.modules):
                if name == module_prefix or name.startswith(f"{module_prefix}."):
                    sys.modules.pop(name, None)
            raise
        return module

    @staticmethod
    def _remove_modules(module_prefix: str) -> None:
        for name in tuple(sys.modules):
            if name == module_prefix or name.startswith(f"{module_prefix}."):
                sys.modules.pop(name, None)

    def _module_prefix(self, record: _PluginRecord) -> str:
        digest = hashlib.sha256(
            f"{record.manifest.id}\0{record.root}".encode()
        ).hexdigest()[:12]
        return f"_ytultimate_ext_{digest}_{next(self._generation)}"

    def _set_error(self, record: _PluginRecord, error: PluginError) -> None:
        with self._state_lock:
            record.status = PluginStatus.ERROR
            record.last_error = error

    def _get_record(self, plugin_id: str) -> _PluginRecord:
        validate_plugin_id(plugin_id)
        record = self._records.get(plugin_id)
        if record is None:
            raise PluginError(
                PluginErrorCode.PLUGIN_NOT_FOUND,
                f"plugin {plugin_id!r} was not discovered",
                plugin_id=plugin_id,
            )
        return record

    def _load_state(self, plugin_id: str) -> PluginState:
        try:
            state = self.state_store.load(plugin_id)
        except PluginError:
            raise
        except Exception as exc:
            raise PluginError(
                PluginErrorCode.STATE_IO,
                "plugin state adapter failed while loading",
                plugin_id=plugin_id,
                details={"exception_type": exc.__class__.__name__},
                cause=exc,
            ) from exc
        if not isinstance(state, PluginState):
            raise PluginError(
                PluginErrorCode.STATE_IO,
                "plugin state adapter returned an invalid value",
                plugin_id=plugin_id,
                details={"expected": "PluginState"},
            )
        return state

    def _save_state(self, plugin_id: str, state: PluginState) -> None:
        try:
            self.state_store.save(plugin_id, state)
        except PluginError:
            raise
        except Exception as exc:
            raise PluginError(
                PluginErrorCode.STATE_IO,
                "plugin state adapter failed while saving",
                plugin_id=plugin_id,
                details={"exception_type": exc.__class__.__name__},
                cause=exc,
            ) from exc


async def _invoke_callable(
    callback: Callable[..., Any],
    *args: Any,
    timeout: float,
) -> Any:
    """Invoke sync or async trusted code with a bounded wait.

    Sync callbacks run in daemon threads. A timed-out thread cannot be killed, but
    it cannot block process exit and plugin registrations are still transactionally
    isolated until setup succeeds.
    """

    started_at = time.monotonic()
    if inspect.iscoroutinefunction(callback):
        return await asyncio.wait_for(
            _await_plugin_code(callback(*args)),
            timeout=timeout,
        )

    value = await _run_sync_daemon(callback, args, timeout)
    if inspect.isawaitable(value):
        remaining = timeout - (time.monotonic() - started_at)
        if remaining <= 0:
            if inspect.iscoroutine(value):
                value.close()
            raise TimeoutError
        return await asyncio.wait_for(_await_plugin_code(value), timeout=remaining)
    return value


async def _await_plugin_code(awaitable: Any) -> Any:
    try:
        return await awaitable
    except asyncio.CancelledError:
        raise
    except BaseException as exc:
        if isinstance(exc, Exception):
            raise
        raise RuntimeError(
            f"plugin callback raised {exc.__class__.__name__}"
        ) from exc


async def _run_sync_daemon(
    callback: Callable[..., Any],
    args: tuple[Any, ...],
    timeout: float,
) -> Any:
    loop = asyncio.get_running_loop()
    future: asyncio.Future[tuple[bool, Any]] = loop.create_future()

    def complete(success: bool, value: Any) -> None:
        if future.done():
            if success and inspect.iscoroutine(value):
                value.close()
            return
        future.set_result((success, value))

    def worker() -> None:
        def deliver(success: bool, value: Any) -> None:
            try:
                loop.call_soon_threadsafe(complete, success, value)
            except RuntimeError:
                # The host event loop may have closed after a timed-out daemon
                # callback. There is no remaining observer to notify.
                if success and inspect.iscoroutine(value):
                    value.close()

        try:
            result = callback(*args)
        except BaseException as exc:  # delivered and re-raised on the event loop.
            deliver(False, exc)
        else:
            deliver(True, result)

    thread = threading.Thread(
        target=worker,
        name=f"plugin:{getattr(callback, '__name__', 'callback')}",
        daemon=True,
    )
    thread.start()
    success, value = await asyncio.wait_for(future, timeout=timeout)
    if success:
        return value
    if isinstance(value, Exception):
        raise value
    raise RuntimeError(
        f"plugin callback raised {value.__class__.__name__}"
    ) from value


def _install_namespace(name: str, path: Path) -> None:
    module = types.ModuleType(name)
    module.__path__ = [str(path)]  # type: ignore[attr-defined]
    module.__package__ = name
    sys.modules[name] = module


def _validate_manager_timeout(label: str, value: float) -> None:
    if (
        not isinstance(value, (int, float))
        or isinstance(value, bool)
        or not math.isfinite(value)
        or not MIN_HANDLER_TIMEOUT <= value <= MAX_HANDLER_TIMEOUT
    ):
        raise ValueError(
            f"{label} must be between {MIN_HANDLER_TIMEOUT:g} and "
            f"{MAX_HANDLER_TIMEOUT:g} seconds"
        )


def _deep_merge(base: dict[str, Any], updates: Mapping[str, Any]) -> dict[str, Any]:
    for key, value in updates.items():
        if (
            key in base
            and isinstance(base[key], dict)
            and isinstance(value, Mapping)
        ):
            base[key] = _deep_merge(base[key], value)
        else:
            base[key] = _clone_json(value)
    return base


def _clone_json(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {str(key): _clone_json(item) for key, item in value.items()}
    if isinstance(value, list):
        return [_clone_json(item) for item in value]
    return value


__all__ = [
    "PluginDiagnostic",
    "PluginManager",
    "PluginSnapshot",
    "PluginStatus",
]
