from __future__ import annotations

import subprocess
import tempfile
import threading
import time
import unittest
from pathlib import Path

from ytultimate.database import Database
from ytultimate.jobs import (
    ConcurrencyLimiter,
    JobCapacityError,
    JobManager,
    JobNotFoundError,
    JobState,
)


class _FakeProcess:
    def __init__(self) -> None:
        self.terminated = threading.Event()
        self.killed = threading.Event()
        self.returncode: int | None = None

    def poll(self) -> int | None:
        return self.returncode

    def terminate(self) -> None:
        self.returncode = -15
        self.terminated.set()

    def kill(self) -> None:
        self.returncode = -9
        self.killed.set()

    def wait(self, timeout: float | None = None) -> int:
        if not self.terminated.wait(timeout):
            raise subprocess.TimeoutExpired("fake", timeout)
        return self.returncode or 0


class JobManagerTests(unittest.TestCase):
    def test_worker_updates_and_returns_immutable_snapshot(self) -> None:
        with JobManager(max_workers=1, max_active_jobs=2) as manager:

            def worker(context):
                context.update(
                    progress=42,
                    stage="Halfway",
                    metadata={"nested": {"value": 1}},
                )
                return {"path": "song.mp3"}

            submitted = manager.submit("download", worker)
            completed = manager.wait(submitted.id, timeout=2)

            self.assertEqual(completed.status, JobState.COMPLETED)
            self.assertEqual(completed.progress, 100)
            self.assertEqual(completed.result, {"path": "song.mp3"})
            copied = completed.to_dict()
            copied["metadata"]["nested"]["value"] = 99
            self.assertEqual(
                manager.get(submitted.id).metadata["nested"]["value"],
                1,
            )

    def test_exception_is_captured_as_failed_job(self) -> None:
        with JobManager(max_workers=1, max_active_jobs=1) as manager:

            def worker(_context):
                raise RuntimeError("download exploded")

            submitted = manager.submit("download", worker)
            failed = manager.wait(submitted.id, timeout=2)

            self.assertEqual(failed.status, JobState.FAILED)
            self.assertIn("download exploded", failed.error)
            public = failed.to_public_dict()
            self.assertEqual(public["status"], "error")
            self.assertEqual(public["job_status"], "failed")

    def test_public_snapshot_uses_running_phase(self) -> None:
        release = threading.Event()
        updated = threading.Event()
        manager = JobManager(max_workers=1, max_active_jobs=1)
        try:

            def worker(context):
                context.update(progress=25, phase="downloading")
                updated.set()
                while not release.wait(0.01):
                    context.raise_if_cancelled()

            submitted = manager.submit("download", worker)
            self.assertTrue(updated.wait(1))
            public = manager.get(submitted.id).to_public_dict()
            self.assertEqual(public["status"], "downloading")
            self.assertEqual(public["job_status"], "running")
            release.set()
            manager.wait(submitted.id, timeout=2)
        finally:
            release.set()
            manager.shutdown(wait=True)

    def test_capacity_is_bounded(self) -> None:
        release = threading.Event()
        started = threading.Event()
        manager = JobManager(max_workers=1, max_active_jobs=1)
        try:

            def worker(context):
                started.set()
                while not release.wait(0.01):
                    context.raise_if_cancelled()

            manager.submit("blocking", worker)
            self.assertTrue(started.wait(1))
            with self.assertRaises(JobCapacityError):
                manager.submit("overflow", lambda _context: None)
        finally:
            release.set()
            manager.shutdown(wait=True)

    def test_cancel_sets_event_and_terminates_registered_process(self) -> None:
        started = threading.Event()
        process = _FakeProcess()
        manager = JobManager(max_workers=1, max_active_jobs=1)
        try:

            def worker(context):
                context.register_process(process)
                started.set()
                while not context.cancel_event.wait(0.01):
                    pass
                context.raise_if_cancelled()

            submitted = manager.submit("process", worker)
            self.assertTrue(started.wait(1))
            self.assertTrue(manager.cancel(submitted.id))
            completed = manager.wait(submitted.id, timeout=2)

            self.assertTrue(process.terminated.is_set())
            self.assertEqual(completed.status, JobState.CANCELLED)
            self.assertTrue(completed.cancellation_requested)
            self.assertFalse(manager.cancel(submitted.id))
        finally:
            manager.shutdown(wait=True)

    def test_pending_future_can_be_cancelled(self) -> None:
        release = threading.Event()
        first_started = threading.Event()
        manager = JobManager(max_workers=1, max_active_jobs=2)
        try:

            def first(context):
                first_started.set()
                while not release.wait(0.01):
                    context.raise_if_cancelled()

            first_job = manager.submit("first", first)
            self.assertTrue(first_started.wait(1))
            second = manager.submit("second", lambda _context: "not run")
            self.assertTrue(manager.cancel(second.id))
            cancelled = manager.wait(second.id, timeout=1)
            self.assertEqual(cancelled.status, JobState.CANCELLED)
            release.set()
            self.assertEqual(
                manager.wait(first_job.id, timeout=2).status,
                JobState.COMPLETED,
            )
        finally:
            release.set()
            manager.shutdown(wait=True)

    def test_updates_are_thread_safe(self) -> None:
        with JobManager(max_workers=4, max_active_jobs=8) as manager:

            def worker(context, value):
                for step in range(20):
                    context.update(progress=step, worker=value)
                    time.sleep(0.001)
                return value

            submitted = [
                manager.submit("parallel", worker, value)
                for value in range(8)
            ]
            results = {
                manager.wait(job.id, timeout=3).result
                for job in submitted
            }
            self.assertEqual(results, set(range(8)))
            self.assertTrue(
                all(
                    snapshot.status is JobState.COMPLETED
                    for snapshot in manager.list(kind="parallel")
                )
            )

    def test_optional_database_persists_and_restores_history(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            database = Database(Path(temporary) / "jobs.sqlite3")
            try:
                with JobManager(
                    max_workers=1,
                    max_active_jobs=1,
                    database=database,
                    persistence_interval=0,
                ) as manager:
                    submitted = manager.submit(
                        "download",
                        lambda _context: {"filename": "track.mp3"},
                        metadata={"title": "Track"},
                    )
                    completed = manager.wait(submitted.id, timeout=2)
                restored_manager = JobManager(
                    max_workers=1,
                    max_active_jobs=1,
                    database=database,
                )
                try:
                    restored = restored_manager.get(completed.id)
                    self.assertEqual(restored.status, JobState.COMPLETED)
                    self.assertEqual(restored.result["filename"], "track.mp3")
                    self.assertTrue(restored_manager.remove(completed.id))
                    self.assertEqual(
                        database.fetchone(
                            "SELECT id FROM jobs WHERE id = ?",
                            (completed.id,),
                        ),
                        None,
                    )
                finally:
                    restored_manager.shutdown()
            finally:
                database.close()

    def test_restart_applies_history_limit_to_memory_and_sqlite(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            database = Database(Path(temporary) / "jobs.sqlite3")
            try:
                completed_ids: list[str] = []
                with JobManager(
                    max_workers=1,
                    max_active_jobs=1,
                    history_limit=10,
                    database=database,
                    persistence_interval=0,
                ) as manager:
                    for index in range(5):
                        submitted = manager.submit(
                            "download",
                            lambda _context, value=index: value,
                        )
                        manager.wait(submitted.id, timeout=2)
                        completed_ids.append(submitted.id)

                self.assertEqual(
                    database.fetchone(
                        "SELECT COUNT(*) AS count FROM jobs"
                    )["count"],
                    5,
                )

                restarted = JobManager(
                    max_workers=1,
                    max_active_jobs=1,
                    history_limit=2,
                    database=database,
                )
                try:
                    self.assertEqual(
                        [snapshot.id for snapshot in restarted.list()],
                        list(reversed(completed_ids[-2:])),
                    )
                    with self.assertRaises(JobNotFoundError):
                        restarted.get(completed_ids[0])

                    persisted = database.fetchall(
                        """
                        SELECT id
                        FROM jobs
                        ORDER BY COALESCE(
                            finished_at,
                            updated_at,
                            created_at
                        ) DESC,
                        created_at DESC,
                        id DESC
                        """
                    )
                    self.assertEqual(
                        [str(row["id"]) for row in persisted],
                        list(reversed(completed_ids[-2:])),
                    )
                finally:
                    restarted.shutdown()
            finally:
                database.close()

    def test_history_limit_prunes_sqlite_as_jobs_complete(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            database = Database(Path(temporary) / "jobs.sqlite3")
            try:
                completed_ids: list[str] = []
                with JobManager(
                    max_workers=1,
                    max_active_jobs=1,
                    history_limit=2,
                    database=database,
                    persistence_interval=0,
                ) as manager:
                    for index in range(4):
                        submitted = manager.submit(
                            "download",
                            lambda _context, value=index: value,
                        )
                        manager.wait(submitted.id, timeout=2)
                        completed_ids.append(submitted.id)

                    self.assertEqual(
                        [snapshot.id for snapshot in manager.list()],
                        list(reversed(completed_ids[-2:])),
                    )
                    persisted_ids = {
                        str(row["id"])
                        for row in database.fetchall("SELECT id FROM jobs")
                    }
                    self.assertEqual(persisted_ids, set(completed_ids[-2:]))
            finally:
                database.close()

    def test_restart_recovers_interrupted_rows_before_bounded_restore(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            database = Database(Path(temporary) / "jobs.sqlite3")
            try:
                for index in range(4):
                    created_at = float(index + 1)
                    database.execute(
                        """
                        INSERT INTO jobs(
                            id, kind, status, progress, message, payload_json,
                            result_json, error, cancellable, plugin_id,
                            created_at, started_at, finished_at, updated_at
                        )
                        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                        """,
                        (
                            f"interrupted-{index}",
                            "download",
                            "running",
                            50,
                            "Downloading",
                            '{"phase":"downloading"}',
                            "null",
                            "",
                            1,
                            "",
                            created_at,
                            created_at,
                            None,
                            created_at,
                        ),
                    )

                restarted = JobManager(
                    max_workers=1,
                    max_active_jobs=1,
                    history_limit=2,
                    database=database,
                    clock=lambda: 1000.0,
                )
                try:
                    restored = restarted.list()
                    self.assertEqual(
                        [snapshot.id for snapshot in restored],
                        ["interrupted-3", "interrupted-2"],
                    )
                    self.assertTrue(
                        all(snapshot.status is JobState.FAILED for snapshot in restored)
                    )
                    self.assertTrue(
                        all(
                            "previous application shutdown" in snapshot.error
                            for snapshot in restored
                        )
                    )

                    persisted = database.fetchall(
                        """
                        SELECT id, status, error, cancellable, finished_at
                        FROM jobs
                        ORDER BY created_at DESC
                        """
                    )
                    self.assertEqual(
                        [str(row["id"]) for row in persisted],
                        ["interrupted-3", "interrupted-2"],
                    )
                    self.assertTrue(
                        all(str(row["status"]) == "failed" for row in persisted)
                    )
                    self.assertTrue(
                        all(int(row["cancellable"]) == 0 for row in persisted)
                    )
                    self.assertTrue(
                        all(float(row["finished_at"]) == 1000.0 for row in persisted)
                    )
                finally:
                    restarted.shutdown()
            finally:
                database.close()

    def test_zero_history_limit_removes_terminal_rows_on_restart(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            database = Database(Path(temporary) / "jobs.sqlite3")
            try:
                with JobManager(
                    max_workers=1,
                    max_active_jobs=1,
                    history_limit=1,
                    database=database,
                    persistence_interval=0,
                ) as manager:
                    submitted = manager.submit("download", lambda _context: None)
                    manager.wait(submitted.id, timeout=2)

                restarted = JobManager(
                    max_workers=1,
                    max_active_jobs=1,
                    history_limit=0,
                    database=database,
                )
                try:
                    self.assertEqual(restarted.list(), [])
                    self.assertEqual(
                        database.fetchone(
                            "SELECT COUNT(*) AS count FROM jobs"
                        )["count"],
                        0,
                    )
                finally:
                    restarted.shutdown()
            finally:
                database.close()

    def test_dynamic_concurrency_limiter_reacts_without_reconstruction(self) -> None:
        current_limit = [1]
        limiter = ConcurrencyLimiter(
            lambda: current_limit[0],
            poll_interval=0.01,
        )
        first_started = threading.Event()
        second_started = threading.Event()
        release = threading.Event()
        active = 0
        maximum_active = 0
        counter_lock = threading.Lock()

        def worker(context, marker):
            nonlocal active, maximum_active
            with limiter.slot(context):
                with counter_lock:
                    active += 1
                    maximum_active = max(maximum_active, active)
                marker.set()
                release.wait(1)
                with counter_lock:
                    active -= 1

        with JobManager(max_workers=2, max_active_jobs=2) as manager:
            first = manager.submit("limited", worker, first_started)
            self.assertTrue(first_started.wait(1))
            second = manager.submit("limited", worker, second_started)
            self.assertFalse(second_started.wait(0.05))
            current_limit[0] = 2
            self.assertTrue(second_started.wait(1))
            release.set()
            manager.wait(first.id, timeout=2)
            manager.wait(second.id, timeout=2)
        self.assertEqual(maximum_active, 2)


if __name__ == "__main__":
    unittest.main()
