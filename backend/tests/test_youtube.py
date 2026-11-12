from __future__ import annotations

import json
import os
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
from urllib.error import HTTPError

from ytultimate.config import AppConfig
from ytultimate.services.youtube import YoutubeAccessPolicy, YoutubeFailure


class YoutubeAccessPolicyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.environment = patch.dict(os.environ, {}, clear=True)
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.config = AppConfig.from_env()
        self.runtime_discovery = patch.object(YoutubeAccessPolicy, "_runtime_candidates", return_value=[])
        self.runtime_discovery.start()
        self.addCleanup(self.runtime_discovery.stop)

    def executable(self, name: str = "node") -> Path:
        path = self.root / name
        path.write_text("#!/bin/sh\nexit 0\n")
        path.chmod(0o755)
        return path

    def test_defaults_delegate_client_selection_and_never_read_browser_cookies(self) -> None:
        options = YoutubeAccessPolicy(self.config).options()
        self.assertNotIn("cookiesfrombrowser", options)
        self.assertNotIn("cookiefile", options)
        self.assertNotIn("extractor_args", options)
        self.assertNotIn("http_headers", options)
        self.assertEqual(options["js_runtimes"], {})
        self.assertEqual(options["cachedir"], str(self.config.cache_dir / "yt-dlp"))
        self.assertEqual(options["socket_timeout"], 20)
        self.assertEqual(options["retries"], 2)
        self.assertEqual(options["extractor_retries"], 1)
        self.assertFalse(options["skip_unavailable_fragments"])

    def test_explicit_runtime_path_has_precedence(self) -> None:
        runtime = self.executable()
        with patch.dict(os.environ, {"YTU_JS_RUNTIME": "node", "YTU_JS_RUNTIME_PATH": str(runtime)}):
            self.assertEqual(YoutubeAccessPolicy().options()["js_runtimes"], {
                "node": {"path": str(runtime.resolve())},
            })

    def test_electron_injected_node_path_is_supported(self) -> None:
        runtime = self.executable()
        with patch.dict(os.environ, {"YTU_NODE_PATH": str(runtime)}):
            self.assertEqual(YoutubeAccessPolicy().options()["js_runtimes"], {
                "node": {"path": str(runtime.resolve())},
            })

    def test_discovers_multiple_runtimes_and_skips_nonexecutables(self) -> None:
        node = self.executable()
        deno = self.executable("deno")
        deno.chmod(0o644)
        with patch.object(YoutubeAccessPolicy, "_runtime_candidates", side_effect=lambda name: {
            "node": [node], "deno": [deno],
        }.get(name, [])):
            self.assertEqual(YoutubeAccessPolicy().options()["js_runtimes"], {
                "node": {"path": str(node.resolve())},
            })

    def test_node_wheel_is_discovered_without_path(self) -> None:
        self.runtime_discovery.stop()
        package = self.root / "nodejs_wheel"
        package.mkdir()
        with patch("ytultimate.services.youtube.importlib.util.find_spec", return_value=SimpleNamespace(
            origin=str(package / "__init__.py"),
        )), patch("ytultimate.services.youtube.shutil.which", return_value=None):
            candidates = YoutubeAccessPolicy()._runtime_candidates("node")
        self.assertIn(package / "bin" / "node", candidates)

    def test_invalid_explicit_runtime_has_actionable_secret_free_error(self) -> None:
        for environment in (
            {"YTU_JS_RUNTIME_PATH": "/private/secret"},
            {"YTU_JS_RUNTIME": "secret"},
            {"YTU_JS_RUNTIME": "node", "YTU_JS_RUNTIME_PATH": "/private/secret"},
        ):
            with self.subTest(environment=environment), patch.dict(os.environ, environment):
                with self.assertRaises(YoutubeFailure) as caught:
                    YoutubeAccessPolicy().options()
                self.assertEqual(caught.exception.code, "configuration")
                self.assertNotIn("secret", str(caught.exception))

    def test_explicit_cookie_file_and_browser_profiles(self) -> None:
        cookie_file = self.root / "cookies.txt"
        cookie_file.write_text("# Netscape HTTP Cookie File\n")
        policy = YoutubeAccessPolicy(settings={
            "youtube_cookie_file": str(cookie_file),
        })
        options = policy.options()
        self.assertEqual(options["cookiefile"], str(cookie_file.resolve()))
        options = YoutubeAccessPolicy(settings={
            "youtube_cookies_browser": '["firefox", "Profile One", null, "Personal"]',
        }).options()
        self.assertEqual(options["cookiesfrombrowser"], ("firefox", "Profile One", None, "Personal"))
        self.assertEqual(YoutubeAccessPolicy(settings={
            "youtube_cookies_browser": "chrome+basictext:Profile One",
        }).options()["cookiesfrombrowser"], ("chrome", "Profile One", "BASICTEXT", None))

    def test_malformed_browser_configuration_fails_safely(self) -> None:
        for value in ("[private", "[]", '[42]', '["firefox", 42]', "::"):
            with self.subTest(value=value):
                with self.assertRaises(YoutubeFailure):
                    YoutubeAccessPolicy(settings={"youtube_cookies_browser": value}).options()

    def test_cookie_sources_cannot_be_merged_implicitly(self) -> None:
        with self.assertRaises(YoutubeFailure) as caught:
            YoutubeAccessPolicy(settings={
                "youtube_cookie_file": "/secret/cookies.txt", "youtube_cookies_browser": "firefox",
            }).options()
        self.assertEqual(caught.exception.code, "configuration")
        self.assertNotIn("secret", str(caught.exception))

    def test_live_settings_env_precedence_and_fresh_options(self) -> None:
        settings = {"download_retries": 3, "youtube_extractor_args": '{"youtube":{"player_client":["mweb"]}}'}
        policy = YoutubeAccessPolicy(settings=lambda: settings)
        first = policy.options()
        first["extractor_args"]["youtube"]["player_client"].append("web")
        self.assertEqual(policy.options()["extractor_args"]["youtube"]["player_client"], ["mweb"])
        settings["download_retries"] = 4
        self.assertEqual(policy.options()["retries"], 4)
        with patch.dict(os.environ, {"YTU_DOWNLOAD_RETRIES": "0"}):
            self.assertEqual(policy.options()["retries"], 0)

    def test_provider_configuration_is_not_limited_to_builtin_extractors(self) -> None:
        value = {"youtubepot-bgutilhttp": {"base_url": ["http://localhost:4416"]},
                 "youtube": {"po_token": "mweb.gvs+private-token"}}
        options = YoutubeAccessPolicy(settings={"youtube_extractor_args": value}).options()
        self.assertEqual(options["extractor_args"]["youtube"]["po_token"], ["mweb.gvs+private-token"])
        self.assertIn("youtubepot-bgutilhttp", options["extractor_args"])

    def test_invalid_extractor_args_do_not_echo_credentials(self) -> None:
        for value in ('{"secret":', '[]', '{"youtube":42}', '{"youtube":{"secret":42}}'):
            with self.subTest(value=value):
                with self.assertRaises(YoutubeFailure) as caught:
                    YoutubeAccessPolicy(settings={"youtube_extractor_args": value}).options()
                self.assertNotIn("secret", str(caught.exception))

    def test_retry_and_timeout_values_are_bounded(self) -> None:
        for key, value in (("download_retries", 11), ("download_retries", -1),
                           ("download_retries", 1.5), ("download_retries", True),
                           ("download_socket_timeout", 0), ("download_socket_timeout", 121)):
            with self.subTest(key=key, value=value):
                with self.assertRaises(YoutubeFailure):
                    YoutubeAccessPolicy(settings={key: value}).options()

    def test_invalid_proxy_is_rejected_without_revealing_credentials(self) -> None:
        for proxy in ("file:///private-secret", "http://", "http://secret@host:invalid", "http://host:70000"):
            with self.subTest(proxy=proxy):
                with self.assertRaises(YoutubeFailure) as caught:
                    YoutubeAccessPolicy(settings={"youtube_proxy": proxy}).options()
                self.assertNotIn("secret", str(caught.exception))

    def test_diagnostics_remain_available_when_credentials_are_removed(self) -> None:
        policy = YoutubeAccessPolicy(settings={"youtube_cookie_file": str(self.root / "secret.txt")})
        diagnostics = policy.diagnostics()
        self.assertEqual(diagnostics["configuration_error"]["code"], "configuration")
        self.assertNotIn("secret", json.dumps(diagnostics))

    def test_fingerprint_changes_for_credentials_and_cookie_file_updates(self) -> None:
        cookie_file = self.root / "cookies.txt"
        cookie_file.write_text("session-one")
        settings = {"youtube_cookie_file": str(cookie_file), "youtube_proxy": "http://user:secret@proxy:8080"}
        policy = YoutubeAccessPolicy(settings=settings)
        first = policy.fingerprint()
        self.assertEqual(first, policy.fingerprint())
        self.assertRegex(first, r"^[0-9a-f]{64}$")
        cookie_file.write_text("session-two-with-different-size")
        self.assertNotEqual(first, policy.fingerprint())
        second = policy.fingerprint()
        settings["youtube_proxy"] = "http://user:different@proxy:8080"
        self.assertNotEqual(second, policy.fingerprint())
        diagnostic = json.dumps(policy.diagnostics())
        self.assertNotIn(str(self.root), diagnostic)
        self.assertNotIn("secret", diagnostic)
        self.assertNotIn("different", diagnostic)
        self.assertTrue(policy.diagnostics()["proxy_configured"])


class YoutubeFailureTests(unittest.TestCase):
    def test_http_classification_including_wrapped_upstream_errors(self) -> None:
        for status, code, retryable in ((403, "forbidden", True), (410, "expired", True),
                                       (429, "rate_limited", False), (404, "unavailable", False),
                                       (503, "network", True)):
            with self.subTest(status=status):
                upstream = HTTPError("https://host/media?token=private-secret", status, "error", {
                    "Retry-After": "120",
                }, None)
                self.addCleanup(upstream.close)
                wrapped = RuntimeError("Download failed")
                wrapped.__cause__ = upstream
                failure = YoutubeFailure.from_exception(wrapped)
                self.assertEqual(failure.code, code)
                self.assertEqual(failure.retryable, retryable)
                self.assertNotIn("private-secret", str(failure))
                if status == 429:
                    self.assertEqual(failure.retry_after, 120)

    def test_error_messages_are_classified_and_never_passed_through(self) -> None:
        for message, code in (
            ("This request was detected as a bot", "bot_detected"),
            ("Sign in to confirm you’re not a bot", "bot_detected"),
            ("No supported JavaScript runtime could be found", "javascript_unavailable"),
            ("n challenge solving failed", "javascript_unavailable"),
            ("requested format is not available", "unavailable"),
            ("connection reset by peer", "network"),
            ("unrecognized failure", "extraction"),
        ):
            with self.subTest(message=message):
                failure = YoutubeFailure.from_exception(RuntimeError(
                    message + " https://host/?token=private-secret Cookie: account-secret",
                ))
                self.assertEqual(failure.code, code)
                self.assertNotIn("private-secret", str(failure))
                self.assertNotIn("account-secret", str(failure))

    def test_existing_failures_preserve_classification(self) -> None:
        failure = YoutubeFailure("configuration", "Configuración inválida")
        self.assertIs(YoutubeFailure.from_exception(failure), failure)

    def test_safe_logger_never_emits_upstream_tokens(self) -> None:
        with patch.object(YoutubeAccessPolicy, "_runtime_candidates", return_value=[]):
            logger = YoutubeAccessPolicy().options()["logger"]
        with self.assertLogs("ytultimate.services.youtube", level="WARNING") as logs:
            logger.warning("HTTP Error 403 https://host/media?token=private-secret")
        self.assertIn("forbidden", logs.output[0])
        self.assertNotIn("private-secret", logs.output[0])


if __name__ == "__main__":
    unittest.main()
