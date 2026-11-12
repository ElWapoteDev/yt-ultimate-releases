"""Async event primitives shared by the application and developer plugins.

The bus deliberately has no dependency on FastAPI (or on the plugin runtime).
That keeps domain services usable in tests, CLI tools, and future background
processes.
"""

from __future__ import annotations

import asyncio
import contextvars
import inspect
import itertools
import logging
import math
import re
import threading
import time
import uuid
import weakref
from collections import defaultdict
from dataclasses import dataclass
from types import MappingProxyType
from typing import Any, Awaitable, Callable, Mapping, TypeAlias


EventHandler: TypeAlias = Callable[["Event"], Any | Awaitable[Any]]
_EVENT_NAME_RE = re.compile(r"^[a-zA-Z][a-zA-Z0-9_-]*(?:\.[a-zA-Z0-9_-]+)*$")


@dataclass(frozen=True, slots=True)
class Event:
    """An immutable event envelope.

    ``payload`` is copied at dispatch time so one handler cannot add or remove
    top-level keys seen by another handler. Payload values themselves should be
    treated as immutable JSON-compatible data.
    """

    name: str
    payload: Mapping[str, Any]
    source: str | None
    emitted_at: float
    event_id: str


@dataclass(frozen=True, slots=True)
class EventHandlerError:
    """Serializable information about one failed event handler."""

    subscription_id: str
    handler_name: str
    error_type: str
    message: str
    timed_out: bool = False

    def to_dict(self) -> dict[str, Any]:
        return {
            "subscription_id": self.subscription_id,
            "handler_name": self.handler_name,
            "error_type": self.error_type,
            "message": self.message,
            "timed_out": self.timed_out,
        }


@dataclass(frozen=True, slots=True)
class EventHandlerResult:
    """Outcome of one isolated handler invocation."""

    subscription_id: str
    handler_name: str
    value: Any = None
    error: EventHandlerError | None = None

    @property
    def ok(self) -> bool:
        return self.error is None


@dataclass(frozen=True, slots=True)
class EventDispatchResult:
    """Aggregate dispatch outcome. Handler failures never abort the dispatch."""

    event: Event
    handlers: tuple[EventHandlerResult, ...]

    @property
    def ok(self) -> bool:
        return all(result.ok for result in self.handlers)

    @property
    def errors(self) -> tuple[EventHandlerError, ...]:
        return tuple(
            result.error for result in self.handlers if result.error is not None
        )


@dataclass(frozen=True, slots=True)
class _HandlerRecord:
    subscription_id: str
    event_name: str
    callback: EventHandler
    owner: str | None
    priority: int
    once: bool
    timeout: float | None
    order: int
    handler_name: str


class _NonStandardHandlerError(RuntimeError):
    def __init__(self, original: BaseException) -> None:
        self.original = original
        super().__init__(
            str(original) or f"handler raised {original.__class__.__name__}"
        )


class Subscription:
    """A removable event subscription returned by :meth:`EventBus.subscribe`."""

    __slots__ = ("_bus", "event_name", "id")

    def __init__(self, bus: "EventBus", event_name: str, subscription_id: str) -> None:
        self._bus = weakref.ref(bus)
        self.event_name = event_name
        self.id = subscription_id

    @property
    def active(self) -> bool:
        bus = self._bus()
        return bool(bus and bus.has_subscription(self.id))

    def unsubscribe(self) -> bool:
        bus = self._bus()
        return bool(bus and bus.unsubscribe(self))

    def __enter__(self) -> "Subscription":
        return self

    def __exit__(self, *_: object) -> None:
        self.unsubscribe()


class EventBus:
    """Concurrent, failure-isolated event dispatcher.

    Handlers for a single event run concurrently and results retain deterministic
    priority/registration order. A failing or timed-out handler is represented in
    the returned result; it does not prevent other handlers from running.

    Sync handlers execute in daemon workers. A timeout stops waiting for a sync
    handler but cannot forcibly terminate Python code already running in that
    worker. Plugin code is trusted code, not a sandbox.
    """

    def __init__(
        self,
        *,
        default_timeout: float = 10.0,
        logger: logging.Logger | None = None,
    ) -> None:
        _validate_timeout(default_timeout, "default_timeout")
        self._default_timeout = float(default_timeout)
        self._logger = logger or logging.getLogger("ytultimate.events")
        self._handlers: dict[str, list[_HandlerRecord]] = defaultdict(list)
        self._ids: dict[str, _HandlerRecord] = {}
        self._lock = threading.RLock()
        self._order = itertools.count()

    @property
    def default_timeout(self) -> float:
        return self._default_timeout

    def subscribe(
        self,
        event_name: str,
        callback: EventHandler,
        *,
        owner: str | None = None,
        priority: int = 0,
        once: bool = False,
        timeout: float | None = None,
    ) -> Subscription:
        """Subscribe to an event name or ``"*"`` and return an unsubscribe handle."""

        self._validate_event_name(event_name, wildcard=True)
        if not callable(callback):
            raise TypeError("callback must be callable")
        if owner is not None and not isinstance(owner, str):
            raise TypeError("owner must be a string or None")
        if not isinstance(priority, int) or isinstance(priority, bool):
            raise TypeError("priority must be an integer")
        if not isinstance(once, bool):
            raise TypeError("once must be a boolean")
        if timeout is not None:
            _validate_timeout(timeout, "timeout")

        subscription_id = uuid.uuid4().hex
        handler_name = getattr(
            callback,
            "__qualname__",
            getattr(callback, "__name__", callback.__class__.__name__),
        )
        record = _HandlerRecord(
            subscription_id=subscription_id,
            event_name=event_name,
            callback=callback,
            owner=owner,
            priority=priority,
            once=once,
            timeout=float(timeout) if timeout is not None else None,
            order=next(self._order),
            handler_name=str(handler_name),
        )
        with self._lock:
            self._handlers[event_name].append(record)
            self._ids[subscription_id] = record
        return Subscription(self, event_name, subscription_id)

    def unsubscribe(self, subscription: Subscription | str) -> bool:
        """Remove a subscription by handle or ID. The operation is idempotent."""

        subscription_id = (
            subscription.id if isinstance(subscription, Subscription) else subscription
        )
        if not isinstance(subscription_id, str):
            raise TypeError("subscription must be a Subscription or string ID")
        with self._lock:
            record = self._ids.pop(subscription_id, None)
            if record is None:
                return False
            handlers = self._handlers.get(record.event_name)
            if handlers is not None:
                self._handlers[record.event_name] = [
                    item
                    for item in handlers
                    if item.subscription_id != subscription_id
                ]
                if not self._handlers[record.event_name]:
                    self._handlers.pop(record.event_name, None)
        return True

    def unsubscribe_owner(self, owner: str) -> int:
        """Atomically remove every subscription owned by a component/plugin."""

        with self._lock:
            records = [
                record
                for record in self._ids.values()
                if record.owner == owner
            ]
            ids = {record.subscription_id for record in records}
            for subscription_id in ids:
                self._ids.pop(subscription_id, None)
            for event_name in {record.event_name for record in records}:
                remaining = [
                    record
                    for record in self._handlers.get(event_name, ())
                    if record.subscription_id not in ids
                ]
                if remaining:
                    self._handlers[event_name] = remaining
                else:
                    self._handlers.pop(event_name, None)
            return len(ids)

    def has_subscription(self, subscription_id: str) -> bool:
        with self._lock:
            return subscription_id in self._ids

    def listener_count(self, event_name: str | None = None) -> int:
        with self._lock:
            if event_name is None:
                return len(self._ids)
            return len(self._handlers.get(event_name, ()))

    def clear(self) -> int:
        """Remove all subscriptions and return the number removed."""

        with self._lock:
            count = len(self._ids)
            self._handlers.clear()
            self._ids.clear()
            return count

    async def emit(
        self,
        event_name: str,
        payload: Mapping[str, Any] | None = None,
        *,
        source: str | None = None,
        timeout: float | None = None,
    ) -> EventDispatchResult:
        """Dispatch an event and collect every isolated handler outcome."""

        self._validate_event_name(event_name, wildcard=False)
        if payload is not None and not isinstance(payload, Mapping):
            raise TypeError("payload must be a mapping or None")
        if source is not None and not isinstance(source, str):
            raise TypeError("source must be a string or None")
        if timeout is not None:
            _validate_timeout(timeout, "timeout")

        event = Event(
            name=event_name,
            payload=MappingProxyType(dict(payload or {})),
            source=source,
            emitted_at=time.time(),
            event_id=uuid.uuid4().hex,
        )
        with self._lock:
            records = [
                *self._handlers.get(event_name, ()),
                *self._handlers.get("*", ()),
            ]
        records.sort(key=lambda item: (-item.priority, item.order))

        # Remove once-handlers before invoking them, so a nested emit cannot run
        # the same once-handler twice.
        for record in records:
            if record.once:
                self.unsubscribe(record.subscription_id)

        tasks = [
            asyncio.create_task(
                self._invoke(record, event, timeout),
                name=f"event:{event_name}:{record.subscription_id}",
            )
            for record in records
        ]
        results = tuple(await asyncio.gather(*tasks)) if tasks else ()
        return EventDispatchResult(event=event, handlers=results)

    def emit_nowait(
        self,
        event_name: str,
        payload: Mapping[str, Any] | None = None,
        *,
        source: str | None = None,
        timeout: float | None = None,
    ) -> asyncio.Task[EventDispatchResult]:
        """Schedule an emit in the current event loop."""

        loop = asyncio.get_running_loop()
        return loop.create_task(
            self.emit(event_name, payload, source=source, timeout=timeout),
            name=f"event:{event_name}",
        )

    def emit_sync(
        self,
        event_name: str,
        payload: Mapping[str, Any] | None = None,
        *,
        source: str | None = None,
        timeout: float | None = None,
    ) -> EventDispatchResult:
        """Dispatch outside an async context.

        Calling this inside a running event loop is an error; use ``await emit``.
        """

        try:
            asyncio.get_running_loop()
        except RuntimeError:
            return asyncio.run(
                self.emit(event_name, payload, source=source, timeout=timeout)
            )
        raise RuntimeError("emit_sync cannot run inside an active event loop")

    async def _invoke(
        self,
        record: _HandlerRecord,
        event: Event,
        dispatch_timeout: float | None,
    ) -> EventHandlerResult:
        effective_timeout = (
            dispatch_timeout
            if dispatch_timeout is not None
            else record.timeout
            if record.timeout is not None
            else self._default_timeout
        )
        started_at = time.monotonic()
        try:
            if inspect.iscoroutinefunction(record.callback):
                pending = record.callback(event)
            else:
                pending = _run_sync_daemon(record.callback, event)
            value = await asyncio.wait_for(pending, timeout=effective_timeout)
            if inspect.isawaitable(value):
                remaining = effective_timeout - (time.monotonic() - started_at)
                if remaining <= 0:
                    if inspect.iscoroutine(value):
                        value.close()
                    raise TimeoutError
                value = await asyncio.wait_for(value, timeout=remaining)
            return EventHandlerResult(
                subscription_id=record.subscription_id,
                handler_name=record.handler_name,
                value=value,
            )
        except TimeoutError:
            error = EventHandlerError(
                subscription_id=record.subscription_id,
                handler_name=record.handler_name,
                error_type="timeout",
                message=f"handler exceeded {effective_timeout:g} seconds",
                timed_out=True,
            )
        except asyncio.CancelledError:
            raise
        except _NonStandardHandlerError as exc:
            original = exc.original
            error = EventHandlerError(
                subscription_id=record.subscription_id,
                handler_name=record.handler_name,
                error_type=original.__class__.__name__,
                message=str(original) or original.__class__.__name__,
            )
            self._logger.error(
                "Event handler %s raised %s for %s",
                record.handler_name,
                original.__class__.__name__,
                event.name,
            )
        except BaseException as exc:  # noqa: BLE001 - isolation is the bus contract.
            error = EventHandlerError(
                subscription_id=record.subscription_id,
                handler_name=record.handler_name,
                error_type=exc.__class__.__name__,
                message=str(exc) or exc.__class__.__name__,
            )
            self._logger.exception(
                "Event handler %s failed for %s",
                record.handler_name,
                event.name,
            )
        return EventHandlerResult(
            subscription_id=record.subscription_id,
            handler_name=record.handler_name,
            error=error,
        )

    @staticmethod
    def _validate_event_name(event_name: str, *, wildcard: bool) -> None:
        if not isinstance(event_name, str):
            raise TypeError("event_name must be a string")
        if wildcard and event_name == "*":
            return
        if not _EVENT_NAME_RE.fullmatch(event_name):
            raise ValueError(
                "event_name must contain dot-separated letters, numbers, '_' or '-'"
            )


async def _run_sync_daemon(callback: EventHandler, event: Event) -> Any:
    """Run trusted sync hooks without letting a timeout block process exit."""

    loop = asyncio.get_running_loop()
    future: asyncio.Future[tuple[bool, Any]] = loop.create_future()
    context = contextvars.copy_context()

    def complete(success: bool, value: Any) -> None:
        if future.done():
            if success and inspect.iscoroutine(value):
                value.close()
            return
        future.set_result((success, value))

    def deliver(success: bool, value: Any) -> None:
        try:
            loop.call_soon_threadsafe(complete, success, value)
        except RuntimeError:
            if success and inspect.iscoroutine(value):
                value.close()

    def worker() -> None:
        try:
            value = context.run(callback, event)
        except BaseException as exc:
            deliver(
                False,
                exc if isinstance(exc, Exception) else _NonStandardHandlerError(exc),
            )
        else:
            deliver(True, value)

    threading.Thread(
        target=worker,
        name=f"event:{getattr(callback, '__name__', 'handler')}",
        daemon=True,
    ).start()
    success, value = await future
    if success:
        return value
    raise value


def _validate_timeout(value: float, label: str) -> None:
    if (
        not isinstance(value, (int, float))
        or isinstance(value, bool)
        or not math.isfinite(value)
        or value <= 0
    ):
        raise ValueError(f"{label} must be a finite number greater than zero")


__all__ = [
    "Event",
    "EventBus",
    "EventDispatchResult",
    "EventHandler",
    "EventHandlerError",
    "EventHandlerResult",
    "Subscription",
]
