from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from ytultimate.bootstrap import (
    CA_BUNDLE_ENVIRONMENT_KEYS,
    configure_ca_bundle,
)


class BootstrapTests(unittest.TestCase):
    def test_configures_common_tls_clients_from_existing_bundle(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            bundle = Path(temporary) / "cacert.pem"
            bundle.write_text("test certificate bundle", encoding="utf-8")
            environment: dict[str, str] = {}

            result = configure_ca_bundle(
                environ=environment,
                locator=lambda: str(bundle),
            )

            self.assertEqual(result, bundle.resolve())
            self.assertEqual(
                environment,
                {
                    key: str(bundle.resolve())
                    for key in CA_BUNDLE_ENVIRONMENT_KEYS
                },
            )

    def test_preserves_explicit_environment_values(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            bundle = Path(temporary) / "cacert.pem"
            bundle.write_text("test certificate bundle", encoding="utf-8")
            environment = {
                "SSL_CERT_FILE": "/managed/company-ca.pem",
                "REQUESTS_CA_BUNDLE": "",
            }

            configure_ca_bundle(
                environ=environment,
                locator=lambda: str(bundle),
            )

            self.assertEqual(
                environment["SSL_CERT_FILE"],
                "/managed/company-ca.pem",
            )
            self.assertEqual(environment["REQUESTS_CA_BUNDLE"], "")
            self.assertEqual(
                environment["CURL_CA_BUNDLE"],
                str(bundle.resolve()),
            )

    def test_missing_or_invalid_bundle_does_not_mutate_environment(self) -> None:
        environment = {"EXISTING": "value"}

        with tempfile.TemporaryDirectory() as temporary:
            missing_bundle = Path(temporary) / "missing" / "cacert.pem"
            missing = configure_ca_bundle(
                environ=environment,
                locator=lambda: str(missing_bundle),
            )
        invalid = configure_ca_bundle(
            environ=environment,
            locator=lambda: None,  # type: ignore[return-value]
        )

        self.assertIsNone(missing)
        self.assertIsNone(invalid)
        self.assertEqual(environment, {"EXISTING": "value"})


if __name__ == "__main__":
    unittest.main()
