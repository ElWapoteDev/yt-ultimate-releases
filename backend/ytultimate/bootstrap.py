"""Process bootstrap helpers shared by frozen and source runtimes."""

from __future__ import annotations

import os
from collections.abc import Callable, MutableMapping
from pathlib import Path


CA_BUNDLE_ENVIRONMENT_KEYS = (
    "SSL_CERT_FILE",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
)


def configure_ca_bundle(
    *,
    environ: MutableMapping[str, str] | None = None,
    locator: Callable[[], str] | None = None,
) -> Path | None:
    """Expose certifi's CA bundle without replacing explicit user settings.

    Framework Python and frozen PyInstaller processes do not always inherit a
    usable system certificate path on macOS.  Pointing the common TLS clients
    at certifi's packaged data keeps urllib, Requests and curl-based adapters
    consistent while preserving any environment supplied by an administrator.
    """

    target = os.environ if environ is None else environ
    if locator is None:
        try:
            from certifi import where as locator
        except ImportError:
            return None

    try:
        bundle = Path(locator()).expanduser().resolve()
    except (OSError, RuntimeError, TypeError, ValueError):
        return None
    if not bundle.is_file():
        return None

    bundle_value = str(bundle)
    for key in CA_BUNDLE_ENVIRONMENT_KEYS:
        target.setdefault(key, bundle_value)
    return bundle
