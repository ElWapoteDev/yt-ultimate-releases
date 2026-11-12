"""Advanced, dependency-free workflow journal for Plugin API v1."""

from __future__ import annotations

import asyncio
import re
from collections.abc import Mapping
from typing import Any

from .journal import WorkflowJournal


CLEAR_CONFIRMATION = "BORRAR WORKFLOW JOURNAL"
MAX_RECENT_LIMIT = 500
MAX_EXPORT_LIMIT = 50000
_SOURCE_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")

SUPPORTED_EVENTS = (
    "app.started",
    "app.stopping",
    "download.queued",
    "download.progress",
    "download.media_ready",
    "playlist.queued",
    "playlist.progress",
    "processing.stems.queued",
    "processing.stems.progress",
    "processing.stems.ready",
    "processing.fingerprint.queued",
    "processing.fingerprint.progress",
    "processing.fingerprint.ready",
)
SUMMARY_TRIGGERS = frozenset(
    {
        "app.stopping",
        "download.media_ready",
        "processing.stems.ready",
        "processing.fingerprint.ready",
    }
)
HOOK_OPTIONS = {
    "app.started": {"priority": 50, "once": True, "timeout": 8},
    "app.stopping": {"priority": 50, "once": True, "timeout": 8},
    "download.progress": {"priority": -50, "once": False, "timeout": 4},
    "playlist.progress": {"priority": -50, "once": False, "timeout": 4},
    "processing.stems.progress": {
        "priority": -50,
        "once": False,
        "timeout": 4,
    },
    "processing.fingerprint.progress": {
        "priority": -50,
        "once": False,
        "timeout": 4,
    },
}


def _error(code: str, message: str, *, field: str | None = None) -> dict[str, Any]:
    error: dict[str, Any] = {"code": code, "message": message}
    if field is not None:
        error["field"] = field
    return {"ok": False, "error": error}


def _unknown_fields(
    payload: Mapping[str, Any],
    allowed: frozenset[str],
) -> dict[str, Any] | None:
    unknown = sorted(str(key) for key in payload if key not in allowed)
    if not unknown:
        return None
    return _error(
        "unknown_fields",
        f"Campos no soportados: {', '.join(unknown)}.",
    )


def _optional_filter(
    payload: Mapping[str, Any],
) -> tuple[str | None, str | None, dict[str, Any] | None]:
    event_name = payload.get("event")
    if event_name is not None:
        if not isinstance(event_name, str) or event_name not in SUPPORTED_EVENTS:
            return None, None, _error(
                "invalid_event_filter",
                "event debe ser uno de los eventos soportados por el plugin.",
                field="event",
            )

    source = payload.get("source")
    if source is not None:
        if not isinstance(source, str) or _SOURCE_RE.fullmatch(source) is None:
            return None, None, _error(
                "invalid_source_filter",
                "source debe ser un identificador de 1 a 128 caracteres.",
                field="source",
            )
    return event_name, source, None


def _limit(
    payload: Mapping[str, Any],
    *,
    default: int | None,
    maximum: int,
) -> tuple[int | None, dict[str, Any] | None]:
    value = payload.get("limit", default)
    if value is None and default is None:
        return None, None
    if (
        not isinstance(value, int)
        or isinstance(value, bool)
        or not 1 <= value <= maximum
    ):
        return None, _error(
            "invalid_limit",
            f"limit debe ser un entero entre 1 y {maximum}.",
            field="limit",
        )
    return value, None


def _summary_payload(
    journal: WorkflowJournal,
    api: Any,
    *,
    trigger: str,
    reason: str | None = None,
) -> dict[str, Any]:
    payload = {
        "plugin_id": api.plugin_id,
        "trigger": trigger,
        "journal": journal.summary(api.config),
    }
    if reason is not None:
        payload["reason"] = reason
    return payload


def setup(api):
    """Register hooks, commands and deterministic teardown."""

    journal = WorkflowJournal(api.paths.data, logger=api.log)
    summary_event = f"{api.plugin_id}.summary"

    async def handle_event(event):
        # Exact host-event subscriptions already exclude this namespace. Keep the
        # guard as a defensive invariant if the registration strategy evolves.
        if event.name.startswith(f"{api.plugin_id}."):
            return None

        config = api.config
        entry = await asyncio.to_thread(journal.record, event, config)
        if (
            bool(config.get("emit_summary_events", False))
            and event.name in SUMMARY_TRIGGERS
        ):
            summary = await asyncio.to_thread(
                _summary_payload,
                journal,
                api,
                trigger=event.name,
            )
            await api.emit(summary_event, summary, timeout=3)
        return entry

    def register_event(event_name):
        options = HOOK_OPTIONS.get(
            event_name,
            {"priority": 0, "once": False, "timeout": 6},
        )
        api.register_hook(
            event_name,
            handle_event,
            priority=options["priority"],
            once=options["once"],
            timeout=options["timeout"],
        )

    for supported_event in SUPPORTED_EVENTS:
        register_event(supported_event)

    @api.register_command(
        "status",
        title="Estado del workflow journal",
        description="Devuelve contadores, rutas y configuración efectiva.",
        timeout=8,
    )
    def status(payload):
        error = _unknown_fields(payload, frozenset())
        if error:
            return error
        return {"ok": True, "status": journal.status(api.config)}

    @api.register_command(
        "recent",
        title="Eventos recientes",
        description="Consulta entradas recientes con filtros exactos validados.",
        timeout=10,
    )
    def recent(payload):
        error = _unknown_fields(
            payload,
            frozenset({"limit", "event", "source"}),
        )
        if error:
            return error
        limit, error = _limit(payload, default=50, maximum=MAX_RECENT_LIMIT)
        if error:
            return error
        event_name, source, error = _optional_filter(payload)
        if error:
            return error
        entries = journal.recent(
            limit=limit,
            event_name=event_name,
            source=source,
        )
        return {
            "ok": True,
            "count": len(entries),
            "filter": {
                "limit": limit,
                "event": event_name,
                "source": source,
            },
            "entries": entries,
        }

    @api.register_command(
        "export",
        title="Exportar workflow journal",
        description="Crea una copia JSONL privada y atómica con filtros opcionales.",
        timeout=30,
    )
    def export(payload):
        error = _unknown_fields(
            payload,
            frozenset({"limit", "event", "source"}),
        )
        if error:
            return error
        limit, error = _limit(payload, default=None, maximum=MAX_EXPORT_LIMIT)
        if error:
            return error
        event_name, source, error = _optional_filter(payload)
        if error:
            return error
        return {
            "ok": True,
            "export": journal.export(
                limit=limit,
                event_name=event_name,
                source=source,
            ),
        }

    @api.register_command(
        "clear",
        title="Vaciar workflow journal",
        description="Elimina entradas sólo con la frase de confirmación exacta.",
        timeout=10,
    )
    def clear(payload):
        error = _unknown_fields(payload, frozenset({"confirm"}))
        if error:
            return error
        if payload.get("confirm") != CLEAR_CONFIRMATION:
            return _error(
                "confirmation_required",
                f"confirm debe ser exactamente {CLEAR_CONFIRMATION!r}.",
                field="confirm",
            )
        return {"ok": True, "clear": journal.clear()}

    @api.register_command(
        "emit-summary",
        title="Emitir resumen de diagnóstico",
        description=f"Emite el evento {summary_event} y devuelve su resultado.",
        timeout=12,
    )
    async def emit_summary(payload):
        error = _unknown_fields(payload, frozenset({"reason"}))
        if error:
            return error
        reason = payload.get("reason")
        if reason is not None:
            if not isinstance(reason, str):
                return _error(
                    "invalid_reason",
                    "reason debe ser texto.",
                    field="reason",
                )
            reason = " ".join(reason.split())
            if not 1 <= len(reason) <= 200:
                return _error(
                    "invalid_reason",
                    "reason debe contener entre 1 y 200 caracteres.",
                    field="reason",
                )

        summary = await asyncio.to_thread(
            _summary_payload,
            journal,
            api,
            trigger="command",
            reason=reason,
        )
        dispatch = await api.emit(summary_event, summary, timeout=5)
        return {
            "ok": True,
            "event": summary_event,
            "event_id": dispatch.event.event_id,
            "dispatch_ok": dispatch.ok,
            "handlers": len(dispatch.handlers),
            "errors": [item.to_dict() for item in dispatch.errors],
            "summary": summary,
        }

    @api.on_shutdown
    def shutdown():
        journal.close()
        api.log.info("Workflow Journal cerrado correctamente")
