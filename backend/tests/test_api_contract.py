from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from starlette.routing import Mount

from ytultimate.app import API_PREFIX, create_app
from test_app import test_config


class APIContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.app = create_app(test_config(Path(self.temporary.name), token=""))
        self.routes = {
            (method, route.path)
            for route in self.app.routes
            for method in (route.methods or set())
        }

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_required_frontend_and_rest_routes_exist(self) -> None:
        expected = {
            ("GET", "/api/v1/system/health"),
            ("GET", "/api/v1/system/paths"),
            ("GET", "/api/v1/settings"),
            ("PATCH", "/api/v1/settings"),
            ("POST", "/api/v1/settings"),
            ("GET", "/api/v1/library"),
            ("POST", "/api/v1/library/import"),
            ("DELETE", "/api/v1/library/{identifier}"),
            ("GET", "/api/v1/library/stats"),
            ("POST", "/api/v1/library/clean"),
            ("GET", "/api/v1/library/tracks/{track_id}/content"),
            ("GET", "/api/v1/library/tracks/{track_id}/artwork"),
            ("GET", "/api/v1/playlists"),
            ("POST", "/api/v1/playlists"),
            ("POST", "/api/v1/playlists/add"),
            ("POST", "/api/v1/playlists/remove"),
            ("GET", "/api/v1/playlists/details"),
            ("PATCH", "/api/v1/playlists/{playlist_id}"),
            ("POST", "/api/v1/playlists/{playlist_id}/tracks"),
            ("GET", "/api/v1/search"),
            ("GET", "/api/v1/playlist_info"),
            ("POST", "/api/v1/download_quick"),
            ("POST", "/api/v1/download_playlist"),
            ("GET", "/api/v1/downloads"),
            ("POST", "/api/v1/downloads/{job_id}/cancel"),
            ("POST", "/api/v1/downloads/clear_completed"),
            ("GET", "/api/v1/stream"),
            ("GET", "/api/v1/stream/proxy"),
            ("GET", "/api/v1/separator/info"),
            ("GET", "/api/v1/separator/jobs"),
            ("POST", "/api/v1/separator/jobs"),
            ("GET", "/api/v1/fingerprint/jobs"),
            ("POST", "/api/v1/fingerprint/jobs"),
            ("GET", "/api/v1/chipmunk/jobs"),
            ("POST", "/api/v1/chipmunk/jobs"),
            ("GET", "/api/v1/developer/plugins"),
            ("POST", "/api/v1/developer/plugins/reload"),
            ("GET", "/api/v1/developer/plugins/examples"),
            (
                "POST",
                "/api/v1/developer/plugins/examples/{example_id}/install",
            ),
            ("PUT", "/api/v1/developer/plugins/{plugin_id}/enabled"),
            (
                "POST",
                "/api/v1/developer/plugins/{plugin_id}/commands/{command_name}",
            ),
            ("POST", "/api/v1/developer/plugins/scaffold"),
        }
        self.assertTrue(expected.issubset(self.routes), expected - self.routes)

    def test_no_unversioned_api_aliases_or_library_mount(self) -> None:
        service_names = {
            "health",
            "settings",
            "library",
            "playlists",
            "downloads",
            "search",
            "stream",
        }
        for route in self.app.routes:
            if route.path == "/api/v1/openapi.json":
                continue
            self.assertTrue(route.path.startswith(f"{API_PREFIX}/"))
            self.assertNotIn(route.path.strip("/"), service_names)
            self.assertFalse(isinstance(route, Mount))

    def test_openapi_has_one_versioned_surface(self) -> None:
        schema = self.app.openapi()
        self.assertEqual(schema["info"]["version"], "4.5.1")
        self.assertTrue(schema["paths"])
        self.assertTrue(
            all(path.startswith(f"{API_PREFIX}/") for path in schema["paths"])
        )
        operation_ids = [
            operation["operationId"]
            for path_item in schema["paths"].values()
            for operation in path_item.values()
            if isinstance(operation, dict) and "operationId" in operation
        ]
        self.assertEqual(len(operation_ids), len(set(operation_ids)))


if __name__ == "__main__":
    unittest.main()
