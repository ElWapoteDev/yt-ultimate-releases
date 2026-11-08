from __future__ import annotations

import asyncio
import json
import os
import shutil
import tempfile
import threading
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest import mock

from ytultimate import app as app_module
from ytultimate.app import create_app
from ytultimate.config import AppConfig


PLUGIN_EXAMPLES = Path(__file__).resolve().parents[1] / "plugin-examples"


def test_config(root: Path, *, token: str = "test-secret") -> AppConfig:
    return AppConfig(
        host="127.0.0.1",
        port=0,
        auth_token=token,
        resource_dir=root / "resources",
        data_dir=root / "data",
        cache_dir=root / "cache",
        log_dir=root / "logs",
        library_dir=root / "library",
        model_dir=root / "models",
        plugin_dir=root / "plugins",
        database_path=root / "data" / "app.sqlite3",
        ffmpeg_path="",
        ffprobe_path="",
        separator_path="",
        developer_mode_override=None,
        allowed_origins=("null", "http://127.0.0.1:5173"),
    )


class ASGIResponse:
    def __init__(self, messages: list[dict[str, Any]]) -> None:
        start = next(
            message
            for message in messages
            if message["type"] == "http.response.start"
        )
        self.status_code = int(start["status"])
        self.headers = {
            key.decode("latin-1").lower(): value.decode("latin-1")
            for key, value in start.get("headers", [])
        }
        self.content = b"".join(
            message.get("body", b"")
            for message in messages
            if message["type"] == "http.response.body"
        )

    def json(self) -> Any:
        return json.loads(self.content)


async def asgi_request(
    app: Any,
    method: str,
    path: str,
    *,
    query: str = "",
    headers: dict[str, str] | None = None,
    json_body: Any = None,
) -> ASGIResponse:
    body = (
        json.dumps(json_body, separators=(",", ":")).encode()
        if json_body is not None
        else b""
    )
    raw_headers = [(b"host", b"testserver")]
    if json_body is not None:
        raw_headers.append((b"content-type", b"application/json"))
    raw_headers.extend(
        (key.lower().encode("latin-1"), value.encode("latin-1"))
        for key, value in (headers or {}).items()
    )
    scope = {
        "type": "http",
        "asgi": {"version": "3.0", "spec_version": "2.3"},
        "http_version": "1.1",
        "method": method.upper(),
        "scheme": "http",
        "path": path,
        "raw_path": path.encode(),
        "query_string": query.encode(),
        "root_path": "",
        "headers": raw_headers,
        "client": ("127.0.0.1", 12345),
        "server": ("testserver", 80),
        "state": {},
    }
    request_sent = False
    messages: list[dict[str, Any]] = []

    async def receive() -> dict[str, Any]:
        nonlocal request_sent
        if not request_sent:
            request_sent = True
            return {
                "type": "http.request",
                "body": body,
                "more_body": False,
            }
        await asyncio.sleep(0)
        return {"type": "http.disconnect"}

    async def send(message: dict[str, Any]) -> None:
        messages.append(message)

    await app(scope, receive, send)
    return ASGIResponse(messages)


class AppFactoryTests(unittest.TestCase):
    def test_factory_is_side_effect_free_until_lifespan(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            app = create_app(test_config(root))
            self.assertFalse((root / "data").exists())
            self.assertFalse((root / "library").exists())
            self.assertIsNotNone(app.state.container)
            self.assertFalse(app.state.container.started)

    def test_closing_stdin_requests_graceful_shutdown_before_exit(self) -> None:
        read_descriptor, write_descriptor = os.pipe()
        server = SimpleNamespace(should_exit=False)
        exited = threading.Event()
        exit_order: list[bool] = []

        def record_exit(_code: int) -> None:
            exit_order.append(server.should_exit)
            exited.set()

        try:
            with (
                mock.patch.object(
                    app_module.sys,
                    "stdin",
                    SimpleNamespace(fileno=lambda: read_descriptor),
                ),
                mock.patch.object(app_module.time, "sleep"),
                mock.patch.object(app_module.os, "_exit", side_effect=record_exit),
            ):
                app_module._shutdown_when_stdin_closes(server)
                os.write(write_descriptor, b"keep-alive")
                self.assertFalse(exited.wait(0.2))
                os.close(write_descriptor)
                write_descriptor = -1
                self.assertTrue(exited.wait(5))
        finally:
            if write_descriptor >= 0:
                os.close(write_descriptor)
            os.close(read_descriptor)
        self.assertEqual(exit_order, [True])


class AppLifespanTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.app = create_app(test_config(self.root))
        self.lifespan = self.app.router.lifespan_context(self.app)
        await self.lifespan.__aenter__()
        self.auth = {"X-YTU-Token": "test-secret"}

    async def asyncTearDown(self) -> None:
        await self.lifespan.__aexit__(None, None, None)
        self.temporary.cleanup()

    async def test_token_health_paths_and_dynamic_settings(self) -> None:
        unauthorized = await asgi_request(
            self.app,
            "GET",
            "/api/v1/system/health",
        )
        self.assertEqual(unauthorized.status_code, 401)

        health = await asgi_request(
            self.app,
            "GET",
            "/api/v1/system/health",
            headers=self.auth,
        )
        self.assertEqual(health.status_code, 200)
        payload = health.json()
        self.assertEqual(payload["status"], "ok")
        self.assertEqual(payload["database_integrity"], "ok")
        self.assertIn("instance_id", payload)
        self.assertEqual(payload["paths"]["library"], str(self.root / "library"))

        updated = await asgi_request(
            self.app,
            "PATCH",
            "/api/v1/settings",
            headers=self.auth,
            json_body={"theme": "dark", "show_advanced_tools": True},
        )
        self.assertEqual(updated.status_code, 200)
        self.assertEqual(updated.json()["theme"], "dark")
        self.assertTrue(updated.json()["show_advanced_tools"])

        enabled = await asgi_request(
            self.app,
            "PATCH",
            "/api/v1/settings",
            headers=self.auth,
            json_body={"developer_mode": True},
        )
        self.assertEqual(enabled.status_code, 200)
        plugins = await asgi_request(
            self.app,
            "GET",
            "/api/v1/developer/plugins",
            headers=self.auth,
        )
        self.assertEqual(plugins.status_code, 200)
        self.assertEqual(plugins.json()["plugins"], [])

    async def test_cors_is_explicit_and_not_wildcard(self) -> None:
        allowed = await asgi_request(
            self.app,
            "OPTIONS",
            "/api/v1/settings",
            headers={
                "Origin": "http://127.0.0.1:5173",
                "Access-Control-Request-Method": "PATCH",
                "Access-Control-Request-Headers": "x-ytu-token,content-type",
            },
        )
        self.assertEqual(allowed.status_code, 200)
        self.assertEqual(
            allowed.headers.get("access-control-allow-origin"),
            "http://127.0.0.1:5173",
        )

        rejected = await asgi_request(
            self.app,
            "OPTIONS",
            "/api/v1/settings",
            headers={
                "Origin": "https://attacker.invalid",
                "Access-Control-Request-Method": "PATCH",
            },
        )
        self.assertNotIn("access-control-allow-origin", rejected.headers)

    async def test_developer_plugin_lifecycle_and_command(self) -> None:
        enabled = await asgi_request(
            self.app,
            "PATCH",
            "/api/v1/settings",
            headers=self.auth,
            json_body={"developer_mode": True},
        )
        self.assertEqual(enabled.status_code, 200)

        scaffold = await asgi_request(
            self.app,
            "POST",
            "/api/v1/developer/plugins/scaffold",
            headers=self.auth,
            json_body={
                "plugin_id": "dev.example.api",
                "name": "API Test",
                "description": "Headless integration test.",
            },
        )
        self.assertEqual(scaffold.status_code, 201)
        self.assertTrue(Path(scaffold.json()["path"]).is_dir())

        activated = await asgi_request(
            self.app,
            "PUT",
            "/api/v1/developer/plugins/dev.example.api/enabled",
            headers=self.auth,
            json_body={"enabled": True},
        )
        self.assertEqual(activated.status_code, 200)
        self.assertEqual(activated.json()["status"], "loaded")

        command = await asgi_request(
            self.app,
            "POST",
            "/api/v1/developer/plugins/dev.example.api/commands/greet",
            headers=self.auth,
            json_body={"arguments": {"name": "Codex"}},
        )
        self.assertEqual(command.status_code, 200)
        self.assertEqual(
            command.json()["result"]["message"],
            "Hola, Codex!",
        )

        disabled = await asgi_request(
            self.app,
            "PATCH",
            "/api/v1/settings",
            headers=self.auth,
            json_body={"developer_mode": False},
        )
        self.assertEqual(disabled.status_code, 200)
        blocked = await asgi_request(
            self.app,
            "GET",
            "/api/v1/developer/plugins",
            headers=self.auth,
        )
        self.assertEqual(blocked.status_code, 403)
        self.assertIsNone(self.app.state.container.plugin_manager)

    async def test_bundled_plugin_example_catalog_and_installation(self) -> None:
        shutil.copytree(
            PLUGIN_EXAMPLES,
            self.root / "resources" / "plugin-examples",
        )
        enabled = await asgi_request(
            self.app,
            "PATCH",
            "/api/v1/settings",
            headers=self.auth,
            json_body={"developer_mode": True},
        )
        self.assertEqual(enabled.status_code, 200)

        catalog = await asgi_request(
            self.app,
            "GET",
            "/api/v1/developer/plugins/examples",
            headers=self.auth,
        )
        self.assertEqual(catalog.status_code, 200)
        self.assertEqual(catalog.json()["count"], 2)
        self.assertEqual(
            {item["level"] for item in catalog.json()["examples"]},
            {"basic", "advanced"},
        )
        self.assertTrue(
            all(not item["installed"] for item in catalog.json()["examples"])
        )

        installed = await asgi_request(
            self.app,
            "POST",
            (
                "/api/v1/developer/plugins/examples/"
                "dev.ytultimate.basic-greeter/install"
            ),
            headers=self.auth,
        )
        self.assertEqual(installed.status_code, 201)
        payload = installed.json()
        self.assertEqual(payload["plugin_id"], "dev.ytultimate.basic-greeter")
        self.assertEqual(payload["plugin"]["status"], "disabled")
        self.assertTrue(
            (
                self.root
                / "plugins"
                / "dev.ytultimate.basic-greeter"
                / "plugin.py"
            ).is_file()
        )

        duplicate = await asgi_request(
            self.app,
            "POST",
            (
                "/api/v1/developer/plugins/examples/"
                "dev.ytultimate.basic-greeter/install"
            ),
            headers=self.auth,
        )
        self.assertEqual(duplicate.status_code, 409)

        catalog_after = await asgi_request(
            self.app,
            "GET",
            "/api/v1/developer/plugins/examples",
            headers=self.auth,
        )
        basic = next(
            item
            for item in catalog_after.json()["examples"]
            if item["id"] == "dev.ytultimate.basic-greeter"
        )
        self.assertTrue(basic["installed"])

    async def test_library_content_uses_id_and_byte_ranges(self) -> None:
        media = self.root / "library" / "sample.mp3"
        media.write_bytes(b"0123456789")
        self.app.state.container.library.reconcile(probe=False)

        listing = await asgi_request(
            self.app,
            "GET",
            "/api/v1/library",
            headers=self.auth,
        )
        self.assertEqual(listing.status_code, 200)
        track = listing.json()[0]
        self.assertEqual(
            track["path"],
            f"/api/v1/library/tracks/{track['id']}/content",
        )
        ranged = await asgi_request(
            self.app,
            "GET",
            track["path"],
            query="token=test-secret",
            headers={"Range": "bytes=2-5"},
        )
        self.assertEqual(ranged.status_code, 206)
        self.assertEqual(ranged.content, b"2345")
        self.assertEqual(ranged.headers["content-range"], "bytes 2-5/10")


if __name__ == "__main__":
    unittest.main()
