from __future__ import annotations

import copy
import json
import subprocess
import threading
import time
import uuid
from concurrent.futures import CancelledError, Future, ThreadPoolExecutor
from contextlib import contextmanager
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Callable, Generic, Iterator, Mapping, Protocol, Sequence, TypeVar


ResultT = TypeVar("ResultT")
Worker = Callable[..., ResultT]
JobListener = Callable[["JobSnapshot"], None]
ConcurrencyLimit = int | Callable[[], int]
INTERRUPTED_JOB_ERROR = "Interrupted by a previous application shutdown"
PERSISTED_TERMINAL_STATUSES = (
    "completed",
    "complete",
    "failed",
    "error",
    "cancelled",
    "canceled",
)


class JobDatabase(Protocol):
    """Minimal persistence boundary implemented by ``ytultimate.Database``."""

    def execute(
        self,
        sql: str,
        parameters: Sequence[Any] | Mapping[str, Any] = (),
    ) -> Any: ...

    def fetchall(
        self,
        sql: str,
        parameters: Sequence[Any] | Mapping[str, Any] = (),
    ) -> Sequence[Mapping[str, Any]]: ...


class JobError(RuntimeError):
    """Base error raised by the background job subsystem."""


class JobNotFoundError(JobError, KeyError):
    """Raised when a job identifier is unknown."""


class JobCapacityError(JobError):
    """Raised when the bounded executor has no room for another job."""


class JobCancelled(JobError):
    """Cooperative cancellation signal used inside job workers."""


class JobState(str, Enum):
    PENDING = "pending"
    RUNNING = "running"
    CANCELLING = "cancelling"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"

    @property
    def terminal(self) -> bool:
        return self in {
            JobState.COMPLETED,
            JobState.FAILED,
            JobState.CANCELLED,
        }


# Kept as a descriptive alias for consumers that prefer "status".
JobStatus = JobState


def _safe_copy(value: Any) -> Any:
    try:
        return copy.deepcopy(value)
    except Exception:
        return value


@dataclass(frozen=True, slots=True)
class JobSnapshot(Generic[ResultT]):
    id: str
    kind: str
    status: JobState
    progress: float
    stage: str
    message: str
    error: str
    metadata: Mapping[str, Any]
    result: ResultT | None
    created_at: float
    started_at: float | None
    updated_at: float
    completed_at: float | None
    cancellation_requested: bool

    @property
    def done(self) -> bool:
        return self.status.terminal

    def to_dict(self, *, flatten_metadata: bool = True) -> dict[str, Any]:
        """Return the canonical job representation.

        ``status`` remains one of :class:`JobState`; HTTP endpoints that retain
        the legacy phase-oriented UI contract should use
        :meth:`to_public_dict` instead.
        """

        data: dict[str, Any] = {
            "id": self.id,
            "kind": self.kind,
            "status": self.status.value,
            "progress": self.progress,
            "stage": self.stage,
            "message": self.message,
            "error": self.error,
            "metadata": _safe_copy(dict(self.metadata)),
            "result": _safe_copy(self.result),
            "created_at": self.created_at,
            "started_at": self.started_at,
            "updated_at": self.updated_at,
            "completed_at": self.completed_at,
            "cancellation_requested": self.cancellation_requested,
        }
        if flatten_metadata:
            for key, value in self.metadata.items():
                data.setdefault(key, _safe_copy(value))
        return data

    def to_public_dict(self) -> dict[str, Any]:
        """Return a flattened, UI-compatible job representation.

        While work is active, ``status`` contains the service phase
        (``downloading``, ``converting``, ``installing``, ``processing``, …).
        The canonical executor state is always retained as ``job_status``.
        Terminal states use the established API spellings ``completed``,
        ``error`` and ``canceled``.
        """

        data = self.to_dict(flatten_metadata=True)
        data["job_status"] = self.status.value
        if self.status is JobState.COMPLETED:
            public_status = "completed"
        elif self.status is JobState.FAILED:
            public_status = "error"
        elif self.status in {JobState.CANCELLED, JobState.CANCELLING}:
            public_status = "canceled"
        elif self.status is JobState.PENDING:
            public_status = "pending"
        else:
            public_status = str(self.metadata.get("phase") or "processing")
        data["status"] = public_status
        return data


@dataclass(slots=True)
class _JobRecord:
    id: str
    kind: str
    status: JobState
    progress: float
    stage: str
    message: str
    error: str
    metadata: dict[str, Any]
    result: Any
    created_at: float
    started_at: float | None
    updated_at: float
    completed_at: float | None
    cancel_event: threading.Event = field(default_factory=threading.Event)
    processes: set[Any] = field(default_factory=set)
    future: Future[Any] | None = None


class JobContext:
    """Capabilities made available to one running worker."""

    __slots__ = ("_manager", "job_id")

    def __init__(self, manager: "JobManager", job_id: str) -> None:
        self._manager = manager
        self.job_id = job_id

    @property
    def cancelled(self) -> bool:
        return self._manager.is_cancellation_requested(self.job_id)

    @property
    def cancel_event(self) -> threading.Event:
        return self._manager.cancellation_event(self.job_id)

    def raise_if_cancelled(self) -> None:
        if self.cancelled:
            raise JobCancelled(f"Job {self.job_id} was cancelled")

    def update(
        self,
        *,
        progress: float | None = None,
        stage: str | None = None,
        message: str | None = None,
        metadata: Mapping[str, Any] | None = None,
        **fields: Any,
    ) -> JobSnapshot:
        return self._manager.update(
            self.job_id,
            progress=progress,
            stage=stage,
            message=message,
            metadata=metadata,
            **fields,
        )

    def register_process(self, process: Any) -> Any:
        self._manager.register_process(self.job_id, process)
        return process

    def unregister_process(self, process: Any) -> None:
        self._manager.unregister_process(self.job_id, process)


class ConcurrencyLimiter:
    """A cancellation-aware gate whose limit may change at runtime."""

    def __init__(
        self,
        limit: ConcurrencyLimit | None,
        *,
        poll_interval: float = 0.25,
    ) -> None:
        if poll_interval <= 0:
            raise ValueError("poll_interval must be positive")
        self._limit = limit
        self._poll_interval = poll_interval
        self._condition = threading.Condition()
        self._active = 0

    @property
    def active(self) -> int:
        with self._condition:
            return self._active

    @property
    def limit(self) -> int | None:
        if self._limit is None:
            return None
        raw = self._limit() if callable(self._limit) else self._limit
        if isinstance(raw, bool):
            raise TypeError("concurrency limit must be an integer")
        try:
            value = int(raw)
        except (TypeError, ValueError) as exc:
            raise ValueError("concurrency limit must be an integer") from exc
        if value < 1:
            raise ValueError("concurrency limit must be at least 1")
        return value

    def acquire(self, context: JobContext) -> None:
        if self._limit is None:
            return
        announced = False
        with self._condition:
            while self._active >= (self.limit or 1):
                if not announced:
                    context.update(
                        stage="Waiting for concurrency slot",
                        phase="pending",
                    )
                    announced = True
                context.raise_if_cancelled()
                self._condition.wait(self._poll_interval)
            context.raise_if_cancelled()
            self._active += 1

    def release(self) -> None:
        if self._limit is None:
            return
        with self._condition:
            if self._active <= 0:
                raise RuntimeError("Concurrency limiter released without acquire")
            self._active -= 1
            self._condition.notify_all()

    @contextmanager
    def slot(self, context: JobContext) -> Iterator[None]:
        self.acquire(context)
        try:
            yield
        finally:
            self.release()


class JobManager:
    """Thread-safe, bounded background work coordinator.

    Workers receive a :class:`JobContext` as their first argument. External
    processes registered through that context are terminated when cancellation
    is requested, while pure-Python workers can observe ``cancel_event``.
    """

    def __init__(
        self,
        *,
        max_workers: int = 4,
        max_active_jobs: int = 64,
        history_limit: int = 500,
        listener: JobListener | None = None,
        database: JobDatabase | None = None,
        persistence_interval: float = 0.5,
        restore_history: bool = True,
        thread_name_prefix: str = "ytu-job",
        clock: Callable[[], float] = time.time,
    ) -> None:
        if max_workers < 1:
            raise ValueError("max_workers must be at least 1")
        if max_active_jobs < max_workers:
            raise ValueError("max_active_jobs must be >= max_workers")
        if history_limit < 0:
            raise ValueError("history_limit cannot be negative")
        if persistence_interval < 0:
            raise ValueError("persistence_interval cannot be negative")

        self._executor = ThreadPoolExecutor(
            max_workers=max_workers,
            thread_name_prefix=thread_name_prefix,
        )
        self._max_active_jobs = max_active_jobs
        self._history_limit = history_limit
        self._listener = listener
        self._database = database
        self._persistence_interval = float(persistence_interval)
        self._last_persisted: dict[str, float] = {}
        self._clock = clock
        self._lock = threading.RLock()
        self._jobs: dict[str, _JobRecord] = {}
        self._closed = False
        if self._database is not None and restore_history:
            self._restore_history()

    def submit(
        self,
        kind: str,
        worker: Worker[ResultT],
        *args: Any,
        metadata: Mapping[str, Any] | None = None,
        job_id: str | None = None,
        **kwargs: Any,
    ) -> JobSnapshot[ResultT]:
        if not kind or not kind.strip():
            raise ValueError("kind cannot be empty")
        if not callable(worker):
            raise TypeError("worker must be callable")

        identifier = job_id or str(uuid.uuid4())
        now = self._clock()
        with self._lock:
            if self._closed:
                raise RuntimeError("JobManager is shut down")
            if identifier in self._jobs:
                raise ValueError(f"Duplicate job id: {identifier}")
            active = sum(not record.status.terminal for record in self._jobs.values())
            if active >= self._max_active_jobs:
                raise JobCapacityError(
                    f"Maximum active job count ({self._max_active_jobs}) reached"
                )

            record = _JobRecord(
                id=identifier,
                kind=kind.strip(),
                status=JobState.PENDING,
                progress=0.0,
                stage="Queued",
                message="",
                error="",
                metadata=dict(metadata or {}),
                result=None,
                created_at=now,
                started_at=None,
                updated_at=now,
                completed_at=None,
            )
            self._jobs[identifier] = record
            self._prune_locked()
            start_gate = threading.Event()
            try:
                future = self._executor.submit(
                    self._execute_after_notification,
                    start_gate,
                    identifier,
                    worker,
                    args,
                    kwargs,
                )
            except Exception:
                self._jobs.pop(identifier, None)
                raise
            record.future = future
            snapshot = self._snapshot_locked(record)

        try:
            self._notify(snapshot)
        finally:
            # A very fast worker must never publish RUNNING/COMPLETED before
            # observers have seen its initial PENDING snapshot.
            start_gate.set()
        return snapshot

    def _execute_after_notification(
        self,
        start_gate: threading.Event,
        job_id: str,
        worker: Worker[Any],
        args: tuple[Any, ...],
        kwargs: Mapping[str, Any],
    ) -> None:
        start_gate.wait()
        self._execute(job_id, worker, args, kwargs)

    def _execute(
        self,
        job_id: str,
        worker: Worker[Any],
        args: tuple[Any, ...],
        kwargs: Mapping[str, Any],
    ) -> None:
        with self._lock:
            record = self._require_locked(job_id)
            if record.cancel_event.is_set():
                snapshot = self._finish_locked(record, JobState.CANCELLED)
                should_run = False
            else:
                now = self._clock()
                record.status = JobState.RUNNING
                record.started_at = now
                record.updated_at = now
                record.stage = record.stage if record.stage != "Queued" else "Running"
                snapshot = self._snapshot_locked(record)
                should_run = True
        self._notify(snapshot)
        if not should_run:
            return

        context = JobContext(self, job_id)
        try:
            result = worker(context, *args, **dict(kwargs))
            with self._lock:
                record = self._require_locked(job_id)
                if record.cancel_event.is_set():
                    snapshot = self._finish_locked(record, JobState.CANCELLED)
                else:
                    record.result = result
                    record.progress = 100.0
                    snapshot = self._finish_locked(record, JobState.COMPLETED)
        except (JobCancelled, CancelledError):
            with self._lock:
                record = self._require_locked(job_id)
                record.cancel_event.set()
                snapshot = self._finish_locked(record, JobState.CANCELLED)
        except BaseException as exc:
            with self._lock:
                record = self._require_locked(job_id)
                if record.cancel_event.is_set():
                    snapshot = self._finish_locked(record, JobState.CANCELLED)
                else:
                    record.error = str(exc) or exc.__class__.__name__
                    snapshot = self._finish_locked(record, JobState.FAILED)
        finally:
            with self._lock:
                record = self._jobs.get(job_id)
                if record is not None:
                    record.processes.clear()
                    self._prune_locked()
        self._notify(snapshot)

    def _finish_locked(
        self,
        record: _JobRecord,
        status: JobState,
    ) -> JobSnapshot:
        now = self._clock()
        record.status = status
        record.updated_at = now
        record.completed_at = now
        if status is JobState.CANCELLED:
            record.stage = "Cancelled"
            record.message = record.message or "Cancellation requested"
        elif status is JobState.COMPLETED:
            record.stage = "Completed"
        elif status is JobState.FAILED:
            record.stage = "Failed"
        return self._snapshot_locked(record)

    def update(
        self,
        job_id: str,
        *,
        progress: float | None = None,
        stage: str | None = None,
        message: str | None = None,
        metadata: Mapping[str, Any] | None = None,
        **fields: Any,
    ) -> JobSnapshot:
        with self._lock:
            record = self._require_locked(job_id)
            if record.status.terminal:
                return self._snapshot_locked(record)
            if progress is not None:
                record.progress = max(0.0, min(100.0, float(progress)))
            if stage is not None:
                record.stage = str(stage)
            if message is not None:
                record.message = str(message)
            if metadata:
                record.metadata.update(dict(metadata))
            if fields:
                record.metadata.update(fields)
            record.updated_at = self._clock()
            snapshot = self._snapshot_locked(record)
        self._notify(snapshot)
        return snapshot

    def get(self, job_id: str) -> JobSnapshot:
        with self._lock:
            return self._snapshot_locked(self._require_locked(job_id))

    snapshot = get

    def list(
        self,
        *,
        kind: str | None = None,
        statuses: set[JobState | str] | None = None,
    ) -> list[JobSnapshot]:
        normalized_statuses = (
            {JobState(status) for status in statuses} if statuses is not None else None
        )
        with self._lock:
            records = [
                record
                for record in self._jobs.values()
                if (kind is None or record.kind == kind)
                and (
                    normalized_statuses is None
                    or record.status in normalized_statuses
                )
            ]
            records.sort(key=lambda item: item.created_at, reverse=True)
            return [self._snapshot_locked(record) for record in records]

    snapshots = list

    def cancel(self, job_id: str) -> bool:
        with self._lock:
            record = self._require_locked(job_id)
            if record.status.terminal:
                return False
            record.cancel_event.set()
            future = record.future
            pending_cancelled = bool(future and future.cancel())
            if record.status is JobState.PENDING and pending_cancelled:
                snapshot = self._finish_locked(record, JobState.CANCELLED)
            else:
                record.status = JobState.CANCELLING
                record.stage = "Cancelling"
                record.message = "Cancellation requested"
                record.updated_at = self._clock()
                snapshot = self._snapshot_locked(record)
            processes = tuple(record.processes)
        self._notify(snapshot)
        for process in processes:
            self._terminate_process(process)
        return True

    def wait(self, job_id: str, timeout: float | None = None) -> JobSnapshot:
        with self._lock:
            record = self._require_locked(job_id)
            future = record.future
        if future is not None:
            try:
                future.result(timeout=timeout)
            except CancelledError:
                pass
        return self.get(job_id)

    def is_cancellation_requested(self, job_id: str) -> bool:
        with self._lock:
            return self._require_locked(job_id).cancel_event.is_set()

    def cancellation_event(self, job_id: str) -> threading.Event:
        with self._lock:
            return self._require_locked(job_id).cancel_event

    def register_process(self, job_id: str, process: Any) -> None:
        terminate_immediately = False
        with self._lock:
            record = self._require_locked(job_id)
            if record.status.terminal or record.cancel_event.is_set():
                terminate_immediately = True
            else:
                record.processes.add(process)
        if terminate_immediately:
            self._terminate_process(process)
            raise JobCancelled(f"Job {job_id} was cancelled")

    def unregister_process(self, job_id: str, process: Any) -> None:
        with self._lock:
            record = self._jobs.get(job_id)
            if record is not None:
                record.processes.discard(process)

    def remove(self, job_id: str) -> bool:
        """Forget one terminal job without affecting its produced artifacts."""

        with self._lock:
            record = self._require_locked(job_id)
            if not record.status.terminal:
                return False
            del self._jobs[job_id]
            self._last_persisted.pop(job_id, None)
        self._delete_persisted((job_id,))
        return True

    def clear(
        self,
        *,
        kind: str | None = None,
        include_failed: bool = True,
    ) -> int:
        with self._lock:
            removable = {
                JobState.COMPLETED,
                JobState.CANCELLED,
            }
            if include_failed:
                removable.add(JobState.FAILED)
            identifiers = [
                identifier
                for identifier, record in self._jobs.items()
                if record.status in removable
                and (kind is None or record.kind == kind)
            ]
            for identifier in identifiers:
                del self._jobs[identifier]
                self._last_persisted.pop(identifier, None)
        self._delete_persisted(identifiers)
        return len(identifiers)

    def shutdown(
        self,
        *,
        wait: bool = True,
        cancel_running: bool = True,
    ) -> None:
        with self._lock:
            self._closed = True
            active_ids = [
                identifier
                for identifier, record in self._jobs.items()
                if not record.status.terminal
            ]
        if cancel_running:
            for identifier in active_ids:
                try:
                    self.cancel(identifier)
                except JobNotFoundError:
                    continue
        self._executor.shutdown(wait=wait, cancel_futures=cancel_running)

    close = shutdown

    def __enter__(self) -> "JobManager":
        return self

    def __exit__(self, *_: object) -> None:
        self.shutdown()

    def _require_locked(self, job_id: str) -> _JobRecord:
        try:
            return self._jobs[job_id]
        except KeyError as exc:
            raise JobNotFoundError(job_id) from exc

    def _snapshot_locked(self, record: _JobRecord) -> JobSnapshot:
        return JobSnapshot(
            id=record.id,
            kind=record.kind,
            status=record.status,
            progress=record.progress,
            stage=record.stage,
            message=record.message,
            error=record.error,
            metadata=_safe_copy(record.metadata),
            result=_safe_copy(record.result),
            created_at=record.created_at,
            started_at=record.started_at,
            updated_at=record.updated_at,
            completed_at=record.completed_at,
            cancellation_requested=record.cancel_event.is_set(),
        )

    def _prune_locked(self) -> None:
        terminal = sorted(
            (
                record
                for record in self._jobs.values()
                if record.status.terminal
            ),
            key=lambda record: (
                (
                    record.completed_at
                    if record.completed_at is not None
                    else record.updated_at
                ),
                record.created_at,
                record.id,
            ),
            reverse=True,
        )
        for record in terminal[self._history_limit :]:
            self._jobs.pop(record.id, None)
            self._last_persisted.pop(record.id, None)

    def _notify(self, snapshot: JobSnapshot) -> None:
        self._persist(snapshot, force=snapshot.done)
        if self._listener is None:
            return
        try:
            self._listener(snapshot)
        except Exception:
            # Observability must never change a job's outcome.
            return

    def _persist(self, snapshot: JobSnapshot, *, force: bool) -> None:
        database = self._database
        if database is None:
            return
        now = self._clock()
        with self._lock:
            last = self._last_persisted.get(snapshot.id, 0.0)
            if (
                not force
                and snapshot.status is not JobState.PENDING
                and now - last < self._persistence_interval
            ):
                return
            self._last_persisted[snapshot.id] = now
        try:
            payload_json = json.dumps(
                dict(snapshot.metadata),
                ensure_ascii=False,
                separators=(",", ":"),
                default=str,
            )
            result_json = json.dumps(
                snapshot.result,
                ensure_ascii=False,
                separators=(",", ":"),
                default=str,
            )
            database.execute(
                """
                INSERT INTO jobs(
                    id, kind, status, progress, message, payload_json,
                    result_json, error, cancellable, plugin_id, created_at,
                    started_at, finished_at, updated_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(id) DO UPDATE SET
                    kind = excluded.kind,
                    status = excluded.status,
                    progress = excluded.progress,
                    message = excluded.message,
                    payload_json = excluded.payload_json,
                    result_json = excluded.result_json,
                    error = excluded.error,
                    cancellable = excluded.cancellable,
                    plugin_id = excluded.plugin_id,
                    started_at = excluded.started_at,
                    finished_at = excluded.finished_at,
                    updated_at = excluded.updated_at
                """,
                (
                    snapshot.id,
                    snapshot.kind,
                    snapshot.status.value,
                    snapshot.progress,
                    snapshot.message,
                    payload_json,
                    result_json,
                    snapshot.error,
                    int(not snapshot.done),
                    str(snapshot.metadata.get("plugin_id") or ""),
                    snapshot.created_at,
                    snapshot.started_at,
                    snapshot.completed_at,
                    snapshot.updated_at,
                ),
            )
        except Exception:
            # Persistence is recoverable via library reconciliation and must not
            # turn successful media work into a failed job.
            return
        if snapshot.done:
            self._prune_persisted_history()

    def _restore_history(self) -> None:
        database = self._database
        if database is None:
            return
        now = self._clock()
        self._recover_interrupted_persisted_jobs(now)
        self._prune_persisted_history()
        if self._history_limit == 0:
            return
        try:
            rows = database.fetchall(
                """
                SELECT id, kind, status, progress, message, payload_json,
                       result_json, error, created_at, started_at, finished_at,
                       updated_at
                FROM jobs
                ORDER BY COALESCE(finished_at, updated_at, created_at) DESC,
                         created_at DESC,
                         id DESC
                LIMIT ?
                """,
                (self._history_limit,),
            )
        except Exception:
            return
        interrupted_records: set[str] = set()
        for row in rows:
            try:
                identifier = str(row["id"])
                state = _restored_state(str(row["status"]))
                metadata = _json_mapping(row["payload_json"])
                result = _json_value(row["result_json"])
                error = str(row["error"] or "")
                completed_at = _optional_timestamp(row["finished_at"])
                interrupted = not state.terminal
                if not state.terminal:
                    state = JobState.FAILED
                    error = error or INTERRUPTED_JOB_ERROR
                    completed_at = now
                    interrupted_records.add(identifier)
                record = _JobRecord(
                    id=identifier,
                    kind=str(row["kind"]),
                    status=state,
                    progress=max(0.0, min(100.0, float(row["progress"] or 0))),
                    stage=str(
                        metadata.get("stage")
                        or (
                            "Completed"
                            if state is JobState.COMPLETED
                            else "Failed"
                            if state is JobState.FAILED
                            else "Cancelled"
                        )
                    ),
                    message=str(row["message"] or ""),
                    error=error,
                    metadata=metadata,
                    result=result,
                    created_at=float(row["created_at"] or now),
                    started_at=_optional_timestamp(row["started_at"]),
                    updated_at=(
                        now if interrupted else float(row["updated_at"] or now)
                    ),
                    completed_at=completed_at,
                )
                if state is JobState.CANCELLED:
                    record.cancel_event.set()
                self._jobs[identifier] = record
            except (KeyError, TypeError, ValueError):
                continue
        self._prune_locked()
        for record in self._jobs.values():
            if record.id in interrupted_records:
                self._persist(self._snapshot_locked(record), force=True)

    def _recover_interrupted_persisted_jobs(self, now: float) -> None:
        """Canonicalize unfinished rows without materializing them in Python."""

        database = self._database
        if database is None:
            return
        placeholders = ", ".join("?" for _ in PERSISTED_TERMINAL_STATUSES)
        try:
            database.execute(
                f"""
                UPDATE jobs
                SET status = ?,
                    error = CASE
                        WHEN TRIM(COALESCE(error, '')) = '' THEN ?
                        ELSE error
                    END,
                    cancellable = 0,
                    finished_at = COALESCE(finished_at, ?),
                    updated_at = ?
                WHERE LOWER(status) NOT IN ({placeholders})
                """,
                (
                    JobState.FAILED.value,
                    INTERRUPTED_JOB_ERROR,
                    now,
                    now,
                    *PERSISTED_TERMINAL_STATUSES,
                ),
            )
        except Exception:
            # A read-only or partially compatible persistence adapter should not
            # prevent the application from starting.
            return

    def _prune_persisted_history(self) -> None:
        """Keep at most ``history_limit`` terminal rows in SQLite."""

        database = self._database
        if database is None:
            return
        placeholders = ", ".join("?" for _ in PERSISTED_TERMINAL_STATUSES)
        try:
            database.execute(
                f"""
                DELETE FROM jobs
                WHERE LOWER(status) IN ({placeholders})
                  AND id NOT IN (
                      SELECT id
                      FROM jobs
                      WHERE LOWER(status) IN ({placeholders})
                      ORDER BY
                          COALESCE(finished_at, updated_at, created_at) DESC,
                          created_at DESC,
                          id DESC
                      LIMIT ?
                  )
                """,
                (
                    *PERSISTED_TERMINAL_STATUSES,
                    *PERSISTED_TERMINAL_STATUSES,
                    self._history_limit,
                ),
            )
        except Exception:
            # Job history is diagnostic state. Failure to prune must not change a
            # download or processing outcome.
            return

    def _delete_persisted(self, identifiers: Sequence[str]) -> None:
        database = self._database
        if database is None:
            return
        for identifier in identifiers:
            try:
                database.execute("DELETE FROM jobs WHERE id = ?", (identifier,))
            except Exception:
                continue

    @staticmethod
    def _terminate_process(process: Any) -> None:
        try:
            if process.poll() is not None:
                return
        except Exception:
            pass
        try:
            process.terminate()
        except (ProcessLookupError, OSError):
            return
        except Exception:
            pass
        try:
            process.wait(timeout=0.75)
            return
        except (subprocess.TimeoutExpired, TimeoutError):
            pass
        except Exception:
            try:
                if process.poll() is not None:
                    return
            except Exception:
                pass
        try:
            process.kill()
        except (ProcessLookupError, OSError):
            pass
        except Exception:
            pass


def _restored_state(value: str) -> JobState:
    aliases = {
        "error": JobState.FAILED,
        "failed": JobState.FAILED,
        "canceled": JobState.CANCELLED,
        "cancelled": JobState.CANCELLED,
        "complete": JobState.COMPLETED,
        "completed": JobState.COMPLETED,
        "pending": JobState.PENDING,
        "queued": JobState.PENDING,
        "cancelling": JobState.CANCELLING,
        "canceling": JobState.CANCELLING,
        "running": JobState.RUNNING,
    }
    # Older versions persisted the UI phase rather than executor state.
    return aliases.get(value.lower(), JobState.RUNNING)


def _json_value(raw: Any) -> Any:
    if raw is None or raw == "":
        return None
    if not isinstance(raw, str):
        return _safe_copy(raw)
    try:
        return json.loads(raw)
    except (TypeError, json.JSONDecodeError):
        return None


def _json_mapping(raw: Any) -> dict[str, Any]:
    value = _json_value(raw)
    return dict(value) if isinstance(value, Mapping) else {}


def _optional_timestamp(value: Any) -> float | None:
    if value is None:
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None
