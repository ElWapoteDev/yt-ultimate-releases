from __future__ import annotations

import json
import logging
import shutil
import tempfile
import unittest
from contextlib import contextmanager
from pathlib import Path

from ytultimate.plugins import (
    MemoryStateStore,
    PluginError,
    PluginErrorCode,
    PluginManager,
    PluginStatus,
    discover_plugin_examples,
    install_plugin_example,
)


EXAMPLES_SOURCE = Path(__file__).resolve().parents[1] / "plugin-examples"
BASIC_PLUGIN_ID = "dev.ytultimate.basic-greeter"
ADVANCED_PLUGIN_ID = "dev.ytultimate.workflow-journal"
EXPECTED_CATALOG = {
    BASIC_PLUGIN_ID: {
        "id": BASIC_PLUGIN_ID,
        "name": "Saludador básico",
        "version": "1.0.0",
        "description": (
            "Ejemplo básico de un saludo configurable y sin efectos "
            "secundarios."
        ),
        "author": "YT Ultimate",
        "level": "basic",
        "permissions": [],
        "installed": False,
    },
    ADVANCED_PLUGIN_ID: {
        "id": ADVANCED_PLUGIN_ID,
        "name": "Workflow Journal",
        "version": "1.0.0",
        "description": (
            "Journal avanzado, configurable y privado para eventos de "
            "descargas y procesamiento."
        ),
        "author": "YT Ultimate",
        "level": "advanced",
        "permissions": [],
        "installed": False,
    },
}
EXPECTED_EXAMPLES = {
    plugin_id: metadata["level"]
    for plugin_id, metadata in EXPECTED_CATALOG.items()
}
ADVANCED_EVENTS = (
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
SUMMARY_EVENT = f"{ADVANCED_PLUGIN_ID}.summary"


@contextmanager
def _temporary_path():
    with tempfile.TemporaryDirectory(
        prefix="ytu-plugin-examples-tests-",
    ) as temporary:
        yield Path(temporary)


def _copy_examples(tmp_path: Path) -> Path:
    target = tmp_path / "examples"
    shutil.copytree(EXAMPLES_SOURCE, target)
    return target


def _file_contents(root: Path) -> dict[str, bytes]:
    return {
        str(file_path.relative_to(root)): file_path.read_bytes()
        for file_path in sorted(root.rglob("*"))
        if file_path.is_file()
    }


def _quiet_logger(name: str) -> logging.Logger:
    logger = logging.getLogger(name)
    logger.handlers.clear()
    logger.addHandler(logging.NullHandler())
    logger.propagate = False
    return logger


def _manager(
    examples_dir: Path,
    tmp_path: Path,
    *,
    logger_name: str,
) -> PluginManager:
    return PluginManager(
        examples_dir,
        developer_mode=True,
        state_store=MemoryStateStore(),
        data_dir=tmp_path / "plugin-data",
        logger=_quiet_logger(logger_name),
    )


class PluginExampleCatalogTests(unittest.TestCase):
    def test_catalog_returns_exact_examples_with_metadata_and_levels(self) -> None:
        with _temporary_path() as tmp_path:
            examples_dir = _copy_examples(tmp_path)
            examples = discover_plugin_examples(examples_dir)

            self.assertEqual(
                {example.manifest.id: example.level for example in examples},
                EXPECTED_EXAMPLES,
            )
            self.assertEqual(
                [example.manifest.id for example in examples],
                sorted(EXPECTED_EXAMPLES),
            )

            for example in examples:
                with self.subTest(plugin_id=example.manifest.id):
                    metadata = example.to_dict(installed=False)
                    self.assertEqual(
                        metadata,
                        EXPECTED_CATALOG[example.manifest.id],
                    )
                    self.assertEqual(example.source.name, example.manifest.id)
                    self.assertTrue((example.source / "plugin.py").is_file())
                    self.assertTrue((example.source / "README.md").is_file())

    def test_catalog_rejects_symlinks_inside_an_example(self) -> None:
        with _temporary_path() as tmp_path:
            examples_dir = _copy_examples(tmp_path)
            unsafe_link = (
                examples_dir
                / BASIC_PLUGIN_ID
                / "unsafe-link"
            )
            try:
                unsafe_link.symlink_to("README.md")
            except (NotImplementedError, OSError) as error:
                self.skipTest(
                    f"La plataforma no permite crear symlinks: {error}",
                )

            with self.assertRaises(PluginError) as caught:
                discover_plugin_examples(examples_dir)
            self.assertEqual(
                caught.exception.code,
                PluginErrorCode.PATH_ESCAPE,
            )

    def test_installation_copies_without_overwriting_and_rejects_missing_id(
        self,
    ) -> None:
        with _temporary_path() as tmp_path:
            examples_dir = _copy_examples(tmp_path)
            plugins_dir = tmp_path / "plugins"

            for plugin_id in EXPECTED_EXAMPLES:
                with self.subTest(plugin_id=plugin_id):
                    installed = install_plugin_example(
                        examples_dir,
                        plugins_dir,
                        plugin_id,
                    )
                    self.assertEqual(installed.name, plugin_id)
                    self.assertEqual(
                        _file_contents(installed),
                        _file_contents(examples_dir / plugin_id),
                    )

            basic_entrypoint = plugins_dir / BASIC_PLUGIN_ID / "plugin.py"
            before = basic_entrypoint.read_bytes()
            with self.assertRaises(PluginError) as caught:
                install_plugin_example(
                    examples_dir,
                    plugins_dir,
                    BASIC_PLUGIN_ID,
                )
            self.assertEqual(
                caught.exception.code,
                PluginErrorCode.SCAFFOLD_EXISTS,
            )
            self.assertEqual(basic_entrypoint.read_bytes(), before)

            missing_id = "dev.ytultimate.missing-example"
            with self.assertRaises(PluginError) as caught:
                install_plugin_example(
                    examples_dir,
                    plugins_dir,
                    missing_id,
                )
            self.assertEqual(
                caught.exception.code,
                PluginErrorCode.PLUGIN_NOT_FOUND,
            )
            self.assertFalse((plugins_dir / missing_id).exists())


class BasicPluginExampleTests(unittest.IsolatedAsyncioTestCase):
    async def test_real_lifecycle_command_config_hook_and_disable(self) -> None:
        with _temporary_path() as tmp_path:
            examples_dir = _copy_examples(tmp_path)
            logger_name = "tests.plugin_examples.basic"
            manager = _manager(
                examples_dir,
                tmp_path,
                logger_name=logger_name,
            )
            logger = logging.getLogger(logger_name)
            manager.discover()

            self.assertEqual(
                manager.snapshot(BASIC_PLUGIN_ID).status,
                PluginStatus.DISABLED,
            )
            self.assertEqual(
                manager.event_bus.listener_count("app.started"),
                0,
            )

            try:
                with self.assertLogs(logger, level="INFO") as captured:
                    enabled = await manager.enable(BASIC_PLUGIN_ID)
                    self.assertEqual(enabled.status, PluginStatus.ACTIVE)
                    self.assertTrue(enabled.enabled)
                    self.assertEqual(
                        [command.id for command in enabled.commands],
                        [f"{BASIC_PLUGIN_ID}.greet"],
                    )

                    default_result = await manager.execute_command(
                        f"{BASIC_PLUGIN_ID}.greet",
                        {},
                    )
                    self.assertEqual(
                        default_result,
                        {
                            "ok": True,
                            "message": "Hola, mundo!",
                            "name": "mundo",
                            "applied_config": {
                                "greeting": "Hola",
                                "punctuation": "!",
                                "uppercase": False,
                            },
                        },
                    )

                    updated = await manager.configure(
                        BASIC_PLUGIN_ID,
                        {
                            "greeting": "Qué tal",
                            "punctuation": "✨",
                            "uppercase": True,
                        },
                    )
                    self.assertEqual(
                        updated,
                        {
                            "greeting": "Qué tal",
                            "punctuation": "✨",
                            "uppercase": True,
                        },
                    )
                    dynamic_result = await manager.execute_command(
                        f"{BASIC_PLUGIN_ID}.greet",
                        {"name": "  Ada   Lovelace  "},
                    )
                    self.assertEqual(
                        dynamic_result["message"],
                        "QUÉ TAL, ADA LOVELACE✨",
                    )
                    self.assertEqual(dynamic_result["name"], "Ada Lovelace")

                    self.assertEqual(
                        manager.event_bus.listener_count("app.started"),
                        1,
                    )
                    dispatch = await manager.event_bus.emit(
                        "app.started",
                        {"version": "4.1.0"},
                        source="test",
                    )
                    self.assertTrue(dispatch.ok, dispatch.errors)
                    self.assertEqual(len(dispatch.handlers), 1)

                    disabled = await manager.disable(BASIC_PLUGIN_ID)
                    self.assertEqual(disabled.status, PluginStatus.DISABLED)
                    self.assertFalse(disabled.enabled)

                self.assertTrue(
                    any(
                        "Saludador básico listo" in message
                        for message in captured.output
                    ),
                )
                self.assertTrue(
                    any(
                        "Saludador básico detenido" in message
                        for message in captured.output
                    ),
                )
                self.assertEqual(
                    manager.event_bus.listener_count("app.started"),
                    0,
                )
                missing_command = await manager.run_command(
                    f"{BASIC_PLUGIN_ID}.greet",
                )
                self.assertFalse(missing_command.ok)
                self.assertEqual(
                    missing_command.error.code,
                    PluginErrorCode.COMMAND_NOT_FOUND,
                )
            finally:
                await manager.stop()


class AdvancedPluginExampleTests(unittest.IsolatedAsyncioTestCase):
    async def test_real_journal_lifecycle_and_commands(self) -> None:
        with _temporary_path() as tmp_path:
            examples_dir = _copy_examples(tmp_path)
            logger_name = "tests.plugin_examples.advanced"
            manager = _manager(
                examples_dir,
                tmp_path,
                logger_name=logger_name,
            )
            logger = logging.getLogger(logger_name)
            manager.discover()
            summaries: list[dict] = []

            def capture_summary(event):
                summaries.append(dict(event.payload))

            summary_subscription = manager.event_bus.subscribe(
                SUMMARY_EVENT,
                capture_summary,
                owner="test",
            )

            try:
                with self.assertLogs(logger, level="INFO") as captured:
                    enabled = await manager.enable(ADVANCED_PLUGIN_ID)
                    self.assertEqual(enabled.status, PluginStatus.ACTIVE)
                    self.assertTrue(enabled.enabled)
                    self.assertEqual(
                        [command.name for command in enabled.commands],
                        [
                            "clear",
                            "emit-summary",
                            "export",
                            "recent",
                            "status",
                        ],
                    )

                    updated = await manager.configure(
                        ADVANCED_PLUGIN_ID,
                        {
                            "emit_summary_events": True,
                            "events": [
                                "app.started",
                                "download.queued",
                                "download.progress",
                                "download.media_ready",
                                "playlist.queued",
                            ],
                            "include_payload": True,
                            "max_entries": 100,
                            "progress_step": 25,
                            "redact_keys": ["token", "authorization"],
                        },
                    )
                    self.assertEqual(updated["max_entries"], 100)
                    self.assertEqual(updated["progress_step"], 25)
                    self.assertTrue(updated["emit_summary_events"])

                    for event_name in ADVANCED_EVENTS:
                        self.assertEqual(
                            manager.event_bus.listener_count(event_name),
                            1,
                            event_name,
                        )

                    started = await manager.event_bus.emit(
                        "app.started",
                        {
                            "version": "4.1.0",
                            "token": "app-secret",
                        },
                        source="test",
                    )
                    self.assertTrue(started.ok, started.errors)
                    self.assertEqual(
                        manager.event_bus.listener_count("app.started"),
                        0,
                    )

                    queued = await manager.event_bus.emit(
                        "download.queued",
                        {
                            "download_id": "download-1",
                            "token": "queue-secret",
                            "nested": {
                                "Authorization": "Bearer secret",
                                "safe": "visible",
                            },
                        },
                        source="test",
                    )
                    self.assertTrue(queued.ok, queued.errors)

                    for progress in (1, 10, 24, 26, 40, 51, 100):
                        dispatch = await manager.event_bus.emit(
                            "download.progress",
                            {
                                "download_id": "download-1",
                                "progress": progress,
                            },
                            source="test",
                        )
                        self.assertTrue(dispatch.ok, dispatch.errors)

                    ready = await manager.event_bus.emit(
                        "download.media_ready",
                        {
                            "download_id": "download-1",
                            "path": "/private/example.mp3",
                        },
                        source="test",
                    )
                    self.assertTrue(ready.ok, ready.errors)
                    self.assertEqual(len(summaries), 1)
                    self.assertEqual(
                        summaries[0]["trigger"],
                        "download.media_ready",
                    )
                    self.assertEqual(
                        summaries[0]["plugin_id"],
                        ADVANCED_PLUGIN_ID,
                    )

                    status = await manager.execute_command(
                        f"{ADVANCED_PLUGIN_ID}.status",
                        {},
                    )
                    self.assertTrue(status["ok"])
                    journal_status = status["status"]
                    self.assertEqual(journal_status["stored_entries"], 7)
                    self.assertEqual(journal_status["total_recorded"], 7)
                    self.assertEqual(journal_status["throttled_progress"], 3)
                    self.assertEqual(
                        journal_status["event_counts"],
                        {
                            "app.started": 1,
                            "download.media_ready": 1,
                            "download.progress": 4,
                            "download.queued": 1,
                        },
                    )
                    data_dir = (
                        tmp_path
                        / "plugin-data"
                        / ADVANCED_PLUGIN_ID
                    ).resolve()
                    self.assertTrue(
                        Path(journal_status["journal_path"])
                        .resolve()
                        .is_relative_to(data_dir),
                    )
                    self.assertTrue(
                        Path(journal_status["state_path"])
                        .resolve()
                        .is_relative_to(data_dir),
                    )

                    recent_queue = await manager.execute_command(
                        f"{ADVANCED_PLUGIN_ID}.recent",
                        {
                            "limit": 1,
                            "event": "download.queued",
                            "source": "test",
                        },
                    )
                    self.assertTrue(recent_queue["ok"])
                    self.assertEqual(recent_queue["count"], 1)
                    queued_payload = recent_queue["entries"][0]["payload"]
                    self.assertEqual(
                        queued_payload["token"],
                        "[REDACTED]",
                    )
                    self.assertEqual(
                        queued_payload["nested"]["Authorization"],
                        "[REDACTED]",
                    )
                    self.assertEqual(
                        queued_payload["nested"]["safe"],
                        "visible",
                    )

                    recent_progress = await manager.execute_command(
                        f"{ADVANCED_PLUGIN_ID}.recent",
                        {
                            "limit": 10,
                            "event": "download.progress",
                            "source": "test",
                        },
                    )
                    self.assertEqual(
                        [
                            entry["payload"]["progress"]
                            for entry in recent_progress["entries"]
                        ],
                        [100, 51, 26, 1],
                    )
                    invalid_limit = await manager.execute_command(
                        f"{ADVANCED_PLUGIN_ID}.recent",
                        {"limit": True},
                    )
                    self.assertFalse(invalid_limit["ok"])
                    self.assertEqual(
                        invalid_limit["error"]["code"],
                        "invalid_limit",
                    )

                    manual_summary = await manager.execute_command(
                        f"{ADVANCED_PLUGIN_ID}.emit-summary",
                        {"reason": "  prueba   manual  "},
                    )
                    self.assertTrue(manual_summary["ok"])
                    self.assertTrue(manual_summary["dispatch_ok"])
                    self.assertEqual(manual_summary["handlers"], 1)
                    self.assertEqual(
                        manual_summary["summary"]["reason"],
                        "prueba manual",
                    )
                    self.assertEqual(len(summaries), 2)

                    for index in range(100):
                        retained = await manager.event_bus.emit(
                            "playlist.queued",
                            {
                                "playlist_id": f"playlist-{index}",
                                "token": f"secret-{index}",
                            },
                            source="retention-test",
                        )
                        self.assertTrue(retained.ok, retained.errors)

                    retained_status = await manager.execute_command(
                        f"{ADVANCED_PLUGIN_ID}.status",
                        {},
                    )
                    retained = retained_status["status"]
                    self.assertEqual(retained["stored_entries"], 100)
                    self.assertEqual(retained["total_recorded"], 107)
                    self.assertEqual(retained["throttled_progress"], 3)
                    self.assertEqual(
                        retained["event_counts"],
                        {"playlist.queued": 100},
                    )

                    recent_playlists = await manager.execute_command(
                        f"{ADVANCED_PLUGIN_ID}.recent",
                        {
                            "limit": 3,
                            "event": "playlist.queued",
                            "source": "retention-test",
                        },
                    )
                    self.assertEqual(
                        [
                            entry["payload"]["playlist_id"]
                            for entry in recent_playlists["entries"]
                        ],
                        ["playlist-99", "playlist-98", "playlist-97"],
                    )
                    self.assertTrue(
                        all(
                            entry["payload"]["token"] == "[REDACTED]"
                            for entry in recent_playlists["entries"]
                        ),
                    )

                    exported = await manager.execute_command(
                        f"{ADVANCED_PLUGIN_ID}.export",
                        {
                            "limit": 3,
                            "event": "playlist.queued",
                            "source": "retention-test",
                        },
                    )
                    self.assertTrue(exported["ok"])
                    export_details = exported["export"]
                    self.assertEqual(export_details["entries"], 3)
                    export_path = Path(export_details["path"]).resolve()
                    self.assertTrue(export_path.is_relative_to(data_dir))
                    self.assertEqual(export_path.parent.name, "exports")
                    exported_entries = [
                        json.loads(line)
                        for line in export_path.read_text(
                            encoding="utf-8",
                        ).splitlines()
                    ]
                    self.assertEqual(
                        [
                            entry["payload"]["playlist_id"]
                            for entry in exported_entries
                        ],
                        ["playlist-97", "playlist-98", "playlist-99"],
                    )

                    reloaded = await manager.reload(ADVANCED_PLUGIN_ID)
                    self.assertEqual(reloaded.status, PluginStatus.ACTIVE)
                    for event_name in ADVANCED_EVENTS:
                        self.assertEqual(
                            manager.event_bus.listener_count(event_name),
                            1,
                            event_name,
                        )
                    persisted_status = await manager.execute_command(
                        f"{ADVANCED_PLUGIN_ID}.status",
                        {},
                    )
                    persisted = persisted_status["status"]
                    self.assertEqual(persisted["stored_entries"], 100)
                    self.assertEqual(persisted["total_recorded"], 107)
                    self.assertEqual(persisted["throttled_progress"], 3)
                    self.assertEqual(
                        persisted["last_export"]["path"],
                        str(export_path),
                    )

                    denied_clear = await manager.execute_command(
                        f"{ADVANCED_PLUGIN_ID}.clear",
                        {},
                    )
                    self.assertFalse(denied_clear["ok"])
                    self.assertEqual(
                        denied_clear["error"]["code"],
                        "confirmation_required",
                    )
                    cleared = await manager.execute_command(
                        f"{ADVANCED_PLUGIN_ID}.clear",
                        {"confirm": "BORRAR WORKFLOW JOURNAL"},
                    )
                    self.assertTrue(cleared["ok"])
                    self.assertEqual(
                        cleared["clear"]["removed_entries"],
                        100,
                    )
                    self.assertTrue(export_path.is_file())

                    empty_status = await manager.execute_command(
                        f"{ADVANCED_PLUGIN_ID}.status",
                        {},
                    )
                    self.assertEqual(
                        empty_status["status"]["stored_entries"],
                        0,
                    )
                    self.assertEqual(
                        empty_status["status"]["total_recorded"],
                        0,
                    )

                    disabled = await manager.disable(ADVANCED_PLUGIN_ID)
                    self.assertEqual(disabled.status, PluginStatus.DISABLED)
                    self.assertFalse(disabled.enabled)
                    for event_name in ADVANCED_EVENTS:
                        self.assertEqual(
                            manager.event_bus.listener_count(event_name),
                            0,
                            event_name,
                        )

                self.assertGreaterEqual(
                    sum(
                        "Workflow Journal cerrado correctamente" in message
                        for message in captured.output
                    ),
                    2,
                )
            finally:
                summary_subscription.unsubscribe()
                await manager.stop()


if __name__ == "__main__":
    unittest.main()
