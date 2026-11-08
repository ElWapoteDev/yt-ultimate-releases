"""Stable in-process developer plugin API, version 1.

Only names exported by this module and ``ytultimate.plugins`` are part of the
public SDK. Runtime implementation classes prefixed with ``_`` are private.
"""

from __future__ import annotations

import logging
import re
import threading
from dataclasses import dataclass
from pathlib import Path, PurePath
from types import MappingProxyType
from typing import Any, Awaitable, Callable, Mapping, TypeAlias, TypeVar, overload

from ytultimate.events import Event, EventBus

from .errors import PluginError, PluginErrorCode
from .manifest import SDK_API_VERSION, PluginManifest


CommandHandler: TypeAlias = Callable[[Mapping[str, Any]], Any | Awaitable[Any]]
HookHandler: TypeAlias = Callable[[Event], Any | Awaitable[Any]]
TeardownHandler: TypeAlias = Callable[[], Any | Awaitable[Any]]
_HandlerT = TypeVar("_HandlerT", bound=Callable[..., Any])
_LOCAL_NAME_RE = re.compile(
    r"^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$",
    re.ASCII,
)
_EVENT_NAME_RE = re.compile(
    r"^[a-zA-Z][a-zA-Z0-9_-]*(?:\.[a-zA-Z0-9_-]+)*$",
    re.ASCII,
)
MIN_HANDLER_TIMEOUT = 0.05
MAX_HANDLER_TIMEOUT = 300.0


@dataclass(frozen=True, slots=True)
class CommandDescriptor:
    """Serializable command metadata suitable for a developer-mode UI."""

    id: str
    plugin_id: str
    name: str
    title: str
    description: str
    timeout: float | None

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "plugin_id": self.plugin_id,
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "timeout": self.timeout,
        }


@dataclass(frozen=True, slots=True)
class CommandExecutionResult:
    """Structured result returned by ``PluginManager.run_command``."""

    command_id: str
    ok: bool
    value: Any = None
    error: PluginError | None = None
    duration_ms: float = 0.0

    def to_dict(self) -> dict[str, Any]:
        result: dict[str, Any] = {
            "command_id": self.command_id,
            "ok": self.ok,
            "duration_ms": round(self.duration_ms, 3),
        }
        if self.ok:
            result["value"] = self.value
        elif self.error is not None:
            result["error"] = self.error.to_dict()
        return result


@dataclass(frozen=True, slots=True)
class PluginPaths:
    """Paths intentionally provided by the host.

    These helpers prevent accidental traversal, but they are not a security
    sandbox: in-process Python plugins can still import ``pathlib`` themselves.
    """

    root: Path
    data: Path

    def data_path(self, *parts: str) -> Path:
        if not parts or any(not isinstance(part, str) or not part for part in parts):
            raise ValueError("data_path requires one or more non-empty string parts")
        relative = PurePath(*parts)
        if relative.is_absolute() or ".." in relative.parts:
            raise ValueError("plugin data paths must remain inside the data directory")
        root = self.data.resolve()
        candidate = root.joinpath(relative).resolve()
        try:
            candidate.relative_to(root)
        except ValueError as exc:
            raise ValueError(
                "plugin data paths must remain inside the data directory"
            ) from exc
        return candidate


@dataclass(frozen=True, slots=True)
class _CommandRegistration:
    descriptor: CommandDescriptor
    handler: CommandHandler


@dataclass(frozen=True, slots=True)
class _HookRegistration:
    event_name: str
    handler: HookHandler
    priority: int
    once: bool
    timeout: float | None


class _RegistrationBuilder:
    """Collects a setup transaction before the manager publishes it."""

    def __init__(self, plugin_id: str) -> None:
        self.plugin_id = plugin_id
        self.commands: list[_CommandRegistration] = []
        self.hooks: list[_HookRegistration] = []
        self.teardowns: list[TeardownHandler] = []
        self._command_names: set[str] = set()
        self._sealed = False
        self._lock = threading.RLock()

    def add_command(self, registration: _CommandRegistration) -> None:
        with self._lock:
            self._ensure_open()
            if registration.descriptor.name in self._command_names:
                raise PluginError(
                    PluginErrorCode.COMMAND_CONFLICT,
                    f"command {registration.descriptor.name!r} is already registered",
                    plugin_id=self.plugin_id,
                )
            self._command_names.add(registration.descriptor.name)
            self.commands.append(registration)

    def add_hook(self, registration: _HookRegistration) -> None:
        with self._lock:
            self._ensure_open()
            self.hooks.append(registration)

    def add_teardown(self, callback: TeardownHandler) -> None:
        with self._lock:
            self._ensure_open()
            self.teardowns.append(callback)

    def seal(self) -> None:
        with self._lock:
            self._sealed = True

    def _ensure_open(self) -> None:
        if self._sealed:
            raise RuntimeError("plugin registrations are only allowed during setup")


class PluginAPI:
    """Host services exposed to a plugin's ``setup(api)`` entrypoint."""

    api_version = SDK_API_VERSION

    def __init__(
        self,
        *,
        manifest: PluginManifest,
        paths: PluginPaths,
        event_bus: EventBus,
        config_getter: Callable[[], Mapping[str, Any]],
        registrations: _RegistrationBuilder,
        logger: logging.Logger | logging.LoggerAdapter,
    ) -> None:
        self.manifest = manifest
        self.paths = paths
        self._event_bus = event_bus
        self._config_getter = config_getter
        self._registrations = registrations
        self.log = logger

    @property
    def plugin_id(self) -> str:
        return self.manifest.id

    @property
    def config(self) -> Mapping[str, Any]:
        """Current validated config as a defensive, top-level read-only copy."""

        return MappingProxyType(_clone_json(self._config_getter()))

    def get_config(self, key: str, default: Any = None) -> Any:
        return _clone_json(self._config_getter().get(key, default))

    @overload
    def register_command(
        self,
        name: str,
        handler: None = None,
        *,
        title: str = "",
        description: str = "",
        timeout: float | None = None,
    ) -> Callable[[CommandHandler], CommandHandler]: ...

    @overload
    def register_command(
        self,
        name: str,
        handler: CommandHandler,
        *,
        title: str = "",
        description: str = "",
        timeout: float | None = None,
    ) -> CommandHandler: ...

    def register_command(
        self,
        name: str,
        handler: CommandHandler | None = None,
        *,
        title: str = "",
        description: str = "",
        timeout: float | None = None,
    ) -> CommandHandler | Callable[[CommandHandler], CommandHandler]:
        """Register ``plugin_id.name``; also works as a decorator."""

        _validate_local_name(name, "command")
        _validate_text(title, "title", max_length=120)
        _validate_text(description, "description", max_length=1_000)
        _validate_timeout(timeout)

        def register(callback: CommandHandler) -> CommandHandler:
            if not callable(callback):
                raise TypeError("command handler must be callable")
            descriptor = CommandDescriptor(
                id=f"{self.plugin_id}.{name}",
                plugin_id=self.plugin_id,
                name=name,
                title=title.strip() or name.replace("_", " ").replace("-", " ").title(),
                description=description.strip(),
                timeout=float(timeout) if timeout is not None else None,
            )
            self._registrations.add_command(
                _CommandRegistration(descriptor=descriptor, handler=callback)
            )
            return callback

        return register if handler is None else register(handler)

    @overload
    def register_hook(
        self,
        event_name: str,
        handler: None = None,
        *,
        priority: int = 0,
        once: bool = False,
        timeout: float | None = None,
    ) -> Callable[[HookHandler], HookHandler]: ...

    @overload
    def register_hook(
        self,
        event_name: str,
        handler: HookHandler,
        *,
        priority: int = 0,
        once: bool = False,
        timeout: float | None = None,
    ) -> HookHandler: ...

    def register_hook(
        self,
        event_name: str,
        handler: HookHandler | None = None,
        *,
        priority: int = 0,
        once: bool = False,
        timeout: float | None = None,
    ) -> HookHandler | Callable[[HookHandler], HookHandler]:
        """Subscribe to a host event when setup commits successfully."""

        if event_name != "*" and not _EVENT_NAME_RE.fullmatch(event_name):
            raise ValueError("invalid event name")
        if not isinstance(priority, int) or isinstance(priority, bool):
            raise TypeError("priority must be an integer")
        if not isinstance(once, bool):
            raise TypeError("once must be a boolean")
        _validate_timeout(timeout)

        def register(callback: HookHandler) -> HookHandler:
            if not callable(callback):
                raise TypeError("hook handler must be callable")
            self._registrations.add_hook(
                _HookRegistration(
                    event_name=event_name,
                    handler=callback,
                    priority=priority,
                    once=once,
                    timeout=float(timeout) if timeout is not None else None,
                )
            )
            return callback

        return register if handler is None else register(handler)

    def on_shutdown(self, callback: _HandlerT) -> _HandlerT:
        """Register cleanup; callbacks run in reverse order on disable/reload."""

        if not callable(callback):
            raise TypeError("shutdown callback must be callable")
        self._registrations.add_teardown(callback)
        return callback

    async def emit(
        self,
        event_name: str,
        payload: Mapping[str, Any] | None = None,
        *,
        timeout: float | None = None,
    ):
        """Emit an application event attributed to this plugin."""

        return await self._event_bus.emit(
            event_name,
            payload,
            source=self.plugin_id,
            timeout=timeout,
        )

    def require_permission(self, permission: str) -> None:
        """Fail clearly if a host capability was not declared in the manifest."""

        if not isinstance(permission, str):
            raise TypeError("permission must be a string")
        if permission not in self.manifest.permissions:
            raise PluginError(
                PluginErrorCode.PERMISSION_NOT_DECLARED,
                f"permission {permission!r} is not declared by this plugin",
                plugin_id=self.plugin_id,
                details={"permission": permission},
            )


def _validate_local_name(value: str, label: str) -> None:
    if (
        not isinstance(value, str)
        or len(value) > 80
        or not _LOCAL_NAME_RE.fullmatch(value)
    ):
        raise ValueError(
            f"{label} name must be lowercase and contain letters, numbers, '.', '_' or '-'"
        )


def _validate_timeout(timeout: float | None) -> None:
    if timeout is None:
        return
    if (
        not isinstance(timeout, (int, float))
        or isinstance(timeout, bool)
        or not MIN_HANDLER_TIMEOUT <= timeout <= MAX_HANDLER_TIMEOUT
    ):
        raise ValueError(
            f"timeout must be between {MIN_HANDLER_TIMEOUT:g} and "
            f"{MAX_HANDLER_TIMEOUT:g} seconds"
        )


def _validate_text(value: str, field: str, *, max_length: int) -> None:
    if not isinstance(value, str):
        raise TypeError(f"{field} must be a string")
    if len(value.strip()) > max_length:
        raise ValueError(f"{field} must not exceed {max_length} characters")


def _clone_json(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {str(key): _clone_json(item) for key, item in value.items()}
    if isinstance(value, list):
        return [_clone_json(item) for item in value]
    return value


__all__ = [
    "CommandDescriptor",
    "CommandExecutionResult",
    "CommandHandler",
    "HookHandler",
    "MAX_HANDLER_TIMEOUT",
    "MIN_HANDLER_TIMEOUT",
    "PluginAPI",
    "PluginPaths",
    "TeardownHandler",
]
