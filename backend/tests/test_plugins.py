from __future__ import annotations

import asyncio
import json
import logging
import tempfile
import unittest
from pathlib import Path

from ytultimate.events import EventBus
from ytultimate.plugins import (
    JsonStateStore,
    MemoryStateStore,
    PluginError,
    PluginErrorCode,
    PluginManager,
    PluginManifest,
    PluginState,
    PluginStatus,
    create_plugin_scaffold,
)


def _manifest(
    plugin_id: str,
    *,
    default_config: dict | None = None,
    config_schema: dict | None = None,
) -> dict:
    return {
        "manifest_version": 1,
        "id": plugin_id,
        "name": "Test Plugin",
        "version": "1.0.0",
        "api_version": 1,
        "entrypoint": "plugin.py:setup",
        "description": "Used by the isolated plugin runtime tests.",
        "permissions": [],
        "default_config": default_config or {},
        "config_schema": config_schema
        or {"type": "object", "additionalProperties": True},
    }


def _write_plugin(
    root: Path,
    plugin_id: str,
    source: str,
    *,
    manifest: dict | None = None,
) -> Path:
    plugin_root = root / plugin_id
    plugin_root.mkdir(parents=True)
    (plugin_root / "plugin.json").write_text(
        json.dumps(manifest or _manifest(plugin_id)),
        encoding="utf-8",
    )
    (plugin_root / "plugin.py").write_text(source, encoding="utf-8")
    return plugin_root


def _quiet_logger(name: str) -> logging.Logger:
    logger = logging.getLogger(name)
    if not logger.handlers:
        logger.addHandler(logging.NullHandler())
    logger.propagate = False
    return logger


class EventBusTests(unittest.IsolatedAsyncioTestCase):
    async def test_failures_timeouts_once_and_unsubscribe_are_isolated(self) -> None:
        bus = EventBus(
            default_timeout=0.05,
            logger=_quiet_logger("tests.plugins.events"),
        )
        calls: list[str] = []

        def good(event):
            calls.append(f"good:{event.payload['value']}")
            return "ok"

        def broken(_event):
            raise RuntimeError("boom")

        async def slow(_event):
            await asyncio.sleep(0.2)

        good_subscription = bus.subscribe("test.event", good, owner="good", priority=5)
        bus.subscribe("test.event", broken)
        bus.subscribe("test.event", slow)
        bus.subscribe("test.event", lambda _event: calls.append("once"), once=True)

        first = await bus.emit("test.event", {"value": 7})
        self.assertFalse(first.ok)
        self.assertEqual(len(first.handlers), 4)
        self.assertEqual(len(first.errors), 2)
        self.assertTrue(any(error.timed_out for error in first.errors))
        self.assertEqual(calls, ["good:7", "once"])

        second = await bus.emit("test.event", {"value": 8})
        self.assertEqual(len(second.handlers), 3)
        self.assertNotIn("once", calls[2:])
        self.assertTrue(good_subscription.unsubscribe())
        self.assertFalse(good_subscription.active)
        bus.subscribe("test.other", good, owner="owned")
        bus.subscribe("test.other", good, owner="owned")
        self.assertEqual(bus.unsubscribe_owner("owned"), 2)
        self.assertEqual(bus.listener_count("test.other"), 0)


class ManifestAndStateTests(unittest.TestCase):
    def test_manifest_and_config_schema_are_validated(self) -> None:
        manifest = PluginManifest.from_dict(
            _manifest(
                "dev.example.config",
                default_config={"quality": 3},
                config_schema={
                    "type": "object",
                    "properties": {
                        "quality": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 5,
                        }
                    },
                    "required": ["quality"],
                    "additionalProperties": False,
                },
            )
        )
        self.assertEqual(manifest.id, "dev.example.config")
        self.assertEqual(manifest.default_config["quality"], 3)
        with self.assertRaises(TypeError):
            manifest.default_config["quality"] = 4

        invalid = _manifest("dev.example.escape")
        invalid["entrypoint"] = "../outside.py:setup"
        with self.assertRaises(PluginError) as caught:
            PluginManifest.from_dict(invalid)
        self.assertEqual(caught.exception.code, PluginErrorCode.INVALID_ENTRYPOINT)

    def test_json_state_store_round_trips_and_rejects_corruption(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            state_path = Path(temporary) / "plugin-state.json"
            store = JsonStateStore(state_path)
            self.assertFalse(store.load("dev.example.state").enabled)
            store.save(
                "dev.example.state",
                PluginState(enabled=True, config={"value": 4}, updated_at=10),
            )
            loaded = store.load("dev.example.state")
            self.assertTrue(loaded.enabled)
            self.assertEqual(loaded.config, {"value": 4})

            state_path.write_text("{bad-json", encoding="utf-8")
            with self.assertRaises(PluginError) as caught:
                store.load("dev.example.state")
            self.assertEqual(caught.exception.code, PluginErrorCode.STATE_IO)

    def test_scaffold_never_overwrites_or_enables(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = create_plugin_scaffold(
                root,
                "dev.example.generated",
                name="Generated",
            )
            manifest = PluginManifest.from_file(target / "plugin.json")
            self.assertEqual(manifest.id, "dev.example.generated")
            self.assertNotIn("enabled", json.loads((target / "plugin.json").read_text()))
            with self.assertRaises(PluginError) as caught:
                create_plugin_scaffold(root, "dev.example.generated")
            self.assertEqual(caught.exception.code, PluginErrorCode.SCAFFOLD_EXISTS)


class PluginManagerTests(unittest.IsolatedAsyncioTestCase):
    async def test_developer_mode_off_never_imports_enabled_code(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            marker = root / "imported"
            _write_plugin(
                root,
                "dev.example.off",
                f"from pathlib import Path\nPath({str(marker)!r}).write_text('bad')\n"
                "def setup(api):\n"
                "    api.register_command('noop', lambda payload: None)\n",
            )
            state = MemoryStateStore()
            state.save(
                "dev.example.off",
                PluginState(enabled=True, config={}),
            )
            manager = PluginManager(
                root,
                developer_mode=False,
                state_store=state,
            )

            snapshots = await manager.start()
            self.assertFalse(marker.exists())
            self.assertEqual(snapshots[0].status, PluginStatus.BLOCKED)
            self.assertEqual(manager.commands(), ())
            with self.assertRaises(PluginError) as caught:
                await manager.enable("dev.example.off")
            self.assertEqual(
                caught.exception.code,
                PluginErrorCode.DEVELOPER_MODE_REQUIRED,
            )
            self.assertFalse(marker.exists())

    async def test_full_lifecycle_commands_hooks_config_reload_and_disable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            schema = {
                "type": "object",
                "properties": {
                    "prefix": {
                        "type": "string",
                        "minLength": 1,
                        "default": "A",
                    }
                },
                "additionalProperties": False,
            }
            source = (
                "def setup(api):\n"
                "    loads = api.paths.data_path('loads.txt')\n"
                "    loads.write_text(loads.read_text() + 'L' if loads.exists() else 'L')\n"
                "    @api.register_command('echo', title='Echo')\n"
                "    def echo(payload):\n"
                "        return {'value': api.config['prefix'] + str(payload.get('value', ''))}\n"
                "    @api.register_hook('test.ping')\n"
                "    def ping(event):\n"
                "        path = api.paths.data_path('hooks.txt')\n"
                "        path.write_text(path.read_text() + 'H' if path.exists() else 'H')\n"
                "    @api.on_shutdown\n"
                "    def stop():\n"
                "        path = api.paths.data_path('stops.txt')\n"
                "        path.write_text(path.read_text() + 'S' if path.exists() else 'S')\n"
            )
            plugin_root = _write_plugin(
                root,
                "dev.example.lifecycle",
                source,
                manifest=_manifest(
                    "dev.example.lifecycle",
                    default_config={"prefix": "A"},
                    config_schema=schema,
                ),
            )
            manager = PluginManager(
                root,
                developer_mode=True,
                logger=_quiet_logger("tests.plugins.lifecycle"),
            )

            discovered = manager.discover()
            self.assertEqual(discovered[0].status, PluginStatus.DISABLED)
            enabled = await manager.enable("dev.example.lifecycle")
            self.assertEqual(enabled.status, PluginStatus.ACTIVE)
            self.assertEqual(
                await manager.execute_command(
                    "dev.example.lifecycle.echo",
                    {"value": 1},
                ),
                {"value": "A1"},
            )

            await manager.event_bus.emit("test.ping")
            data = root / ".data" / "dev.example.lifecycle"
            self.assertEqual((data / "hooks.txt").read_text(), "H")

            manager.set_config("dev.example.lifecycle", {"prefix": "B"})
            self.assertEqual(
                await manager.execute_command(
                    "dev.example.lifecycle.echo",
                    {"value": 2},
                ),
                {"value": "B2"},
            )
            with self.assertRaises(PluginError) as caught:
                manager.set_config("dev.example.lifecycle", {"unknown": True})
            self.assertEqual(caught.exception.code, PluginErrorCode.CONFIG_INVALID)

            changed_source = source.replace(
                "return {'value': api.config['prefix'] + str(payload.get('value', ''))}",
                "return {'value': 'R' + api.config['prefix'] + str(payload.get('value', ''))}",
            )
            (plugin_root / "plugin.py").write_text(changed_source, encoding="utf-8")
            await manager.reload("dev.example.lifecycle")
            self.assertEqual((data / "loads.txt").read_text(), "LL")
            self.assertEqual((data / "stops.txt").read_text(), "S")
            self.assertEqual(
                await manager.execute_command(
                    "dev.example.lifecycle.echo",
                    {"value": 3},
                ),
                {"value": "RB3"},
            )
            disabled = await manager.disable("dev.example.lifecycle")
            self.assertEqual(disabled.status, PluginStatus.DISABLED)
            self.assertFalse(disabled.enabled)
            self.assertEqual((data / "stops.txt").read_text(), "SS")

            await manager.event_bus.emit("test.ping")
            self.assertEqual((data / "hooks.txt").read_text(), "H")
            result = await manager.run_command("dev.example.lifecycle.echo")
            self.assertFalse(result.ok)
            self.assertEqual(result.error.code, PluginErrorCode.COMMAND_NOT_FOUND)

    async def test_setup_failure_rolls_back_registrations(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _write_plugin(
                root,
                "dev.example.failure",
                "def setup(api):\n"
                "    api.register_command('partial', lambda payload: {'bad': True})\n"
                "    raise RuntimeError('setup exploded')\n",
            )
            manager = PluginManager(
                root,
                developer_mode=True,
                logger=_quiet_logger("tests.plugins.setup_failure"),
            )
            manager.discover()

            with self.assertRaises(PluginError) as caught:
                await manager.enable("dev.example.failure")
            self.assertEqual(caught.exception.code, PluginErrorCode.SETUP_FAILED)
            self.assertEqual(manager.commands(), ())
            snapshot = manager.snapshot("dev.example.failure")
            self.assertEqual(snapshot.status, PluginStatus.ERROR)
            self.assertTrue(snapshot.enabled)

    async def test_command_timeout_is_structured(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _write_plugin(
                root,
                "dev.example.timeout",
                "import asyncio\n"
                "def setup(api):\n"
                "    @api.register_command('slow', timeout=0.05)\n"
                "    async def slow(payload):\n"
                "        await asyncio.sleep(0.2)\n",
            )
            manager = PluginManager(root, developer_mode=True)
            manager.discover()
            await manager.enable("dev.example.timeout")
            result = await manager.run_command("dev.example.timeout.slow")
            self.assertFalse(result.ok)
            self.assertEqual(result.error.code, PluginErrorCode.COMMAND_TIMEOUT)
            await manager.stop()


if __name__ == "__main__":
    unittest.main()
