from __future__ import annotations

import threading
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from ytultimate.database import Database


class DatabaseTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = TemporaryDirectory()
        self.database_path = (
            Path(self.temporary_directory.name) / "state" / "library.sqlite3"
        )
        self.database = Database(self.database_path)

    def tearDown(self) -> None:
        self.database.close()
        self.temporary_directory.cleanup()

    def test_initializes_wal_schema_and_is_idempotent(self) -> None:
        journal_mode = self.database.fetchone("PRAGMA journal_mode")
        self.assertEqual(journal_mode[0].lower(), "wal")
        self.assertEqual(
            self.database.schema_version,
            Database.SCHEMA_VERSION,
        )
        table_names = {
            row["name"]
            for row in self.database.fetchall(
                """
                SELECT name
                FROM sqlite_master
                WHERE type = 'table'
                """
            )
        }
        self.assertTrue(
            {
                "tracks",
                "playlists",
                "playlist_tracks",
                "jobs",
                "settings",
                "plugin_states",
            }.issubset(table_names)
        )
        self.assertEqual(self.database.migrate(), Database.SCHEMA_VERSION)
        self.assertEqual(self.database.integrity_check(), "ok")

    def test_transactions_roll_back_and_nested_savepoints_are_isolated(
        self,
    ) -> None:
        with self.assertRaisesRegex(RuntimeError, "rollback"):
            with self.database.transaction():
                self.database.set_setting("outer", {"saved": False})
                raise RuntimeError("rollback")
        self.assertIsNone(self.database.get_setting("outer"))

        with self.database.transaction():
            self.database.set_setting("outer", {"saved": True})
            with self.assertRaises(ValueError):
                with self.database.transaction():
                    self.database.set_setting("nested", 1)
                    raise ValueError("nested rollback")
            self.database.set_setting("after_nested", 2)

        self.assertEqual(
            self.database.get_setting("outer"),
            {"saved": True},
        )
        self.assertIsNone(self.database.get_setting("nested"))
        self.assertEqual(self.database.get_setting("after_nested"), 2)

    def test_setting_helpers_round_trip_json_atomically(self) -> None:
        values = {
            "developer_mode": True,
            "plugin.options": {
                "paths": ["/tmp/a", "/tmp/b"],
                "threshold": 0.25,
            },
        }
        self.database.set_settings(values)
        self.assertEqual(self.database.get_settings(), values)
        self.assertTrue(self.database.delete_setting("developer_mode"))
        self.assertFalse(self.database.delete_setting("developer_mode"))

    def test_serializes_writes_from_application_threads(self) -> None:
        failures: list[BaseException] = []

        def writer(worker: int) -> None:
            try:
                for index in range(25):
                    self.database.set_setting(
                        f"worker.{worker}.{index}",
                        {"worker": worker, "index": index},
                    )
            except BaseException as exc:
                failures.append(exc)

        threads = [
            threading.Thread(target=writer, args=(worker,))
            for worker in range(4)
        ]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()

        self.assertEqual(failures, [])
        self.assertEqual(len(self.database.get_settings()), 100)
        self.assertEqual(self.database.integrity_check(), "ok")


if __name__ == "__main__":
    unittest.main()

