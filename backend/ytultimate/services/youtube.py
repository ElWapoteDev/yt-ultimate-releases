"""Shared, configurable YouTube access for extraction, downloads and playback.

Client selection and challenge solving belong to yt-dlp and its providers. This
module configures their public API without embedding client identities or tokens.
Browser credentials are only read when the user explicitly selects a source.
"""

from __future__ import annotations

import hashlib
import importlib.metadata
import importlib.util
import json
import logging
import os
import re
import shutil
import sys
from collections.abc import Callable, Mapping
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

from platformdirs import user_cache_path

from ..config import APP_AUTHOR, APP_NAME, AppConfig


class YoutubeFailure(RuntimeError):
    """A stable, public failure that never includes upstream URLs or secrets."""

    def __init__(
        self,
        code: str,
        message: str,
        *,
        retryable: bool = False,
        retry_after: int | None = None,
    ) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.retryable = retryable
        self.retry_after = retry_after

    @classmethod
    def from_exception(cls, error: BaseException | str) -> YoutubeFailure:
        if isinstance(error, cls):
            return error
        parts: list[str] = []
        statuses: set[int] = set()
        retry_after = None
        pending: list[BaseException | str] = [error]
        seen: set[int] = set()
        while pending and len(seen) < 16:
            item = pending.pop()
            if id(item) in seen:
                continue
            seen.add(id(item))
            parts.append(f"{type(item).__name__} {item}".lower())
            response = getattr(item, "response", None)
            for candidate in (item, response):
                for field in ("status_code", "status", "code"):
                    value = getattr(candidate, field, None)
                    if isinstance(value, int):
                        statuses.add(value)
                headers = getattr(candidate, "headers", None)
                if headers is not None:
                    try:
                        retry_after = max(0, min(86400, int(headers.get("Retry-After"))))
                    except (TypeError, ValueError, AttributeError):
                        pass
            for field in ("__cause__", "__context__", "cause", "reason"):
                nested = getattr(item, field, None)
                if isinstance(nested, BaseException):
                    pending.append(nested)
            exc_info = getattr(item, "exc_info", None)
            if isinstance(exc_info, tuple) and len(exc_info) > 1:
                if isinstance(exc_info[1], BaseException):
                    pending.append(exc_info[1])

        detail = " ".join(parts)
        if 429 in statuses or re.search(r"\b429\b|too many requests|rate.?limit", detail):
            return cls(
                "rate_limited",
                "YouTube limitó temporalmente las solicitudes. Espera unos minutos "
                "y reduce las descargas simultáneas antes de volver a intentar.",
                retry_after=retry_after,
            )
        if any(term in detail for term in (
            "detected as a bot", "confirm you’re not a bot", "confirm you're not a bot",
            "botdetection", "bot detection", "sign in to confirm", "login required",
        )):
            return cls(
                "bot_detected",
                "YouTube solicita verificar la sesión. Configura explícitamente un archivo "
                "de cookies o un navegador en los ajustes de YouTube; si requiere un "
                "PO Token, configura un proveedor compatible con yt-dlp.",
            )
        if any(term in detail for term in (
            "javascript runtime", "js runtime", "challenge solving failed", "n challenge",
            "nsig extraction failed", "signature extraction failed", "yt-dlp-ejs",
            "ejs challenge",
        )):
            return cls(
                "javascript_unavailable",
                "No se pudo resolver el desafío de YouTube. Actualiza yt-dlp con sus "
                "dependencias default y configura un runtime JavaScript compatible "
                "(Deno o Node) mediante YTU_JS_RUNTIME y YTU_JS_RUNTIME_PATH.",
            )
        if 403 in statuses or re.search(r"\b403\b|\bforbidden\b|po.?token", detail):
            return cls(
                "forbidden",
                "YouTube rechazó el acceso al archivo (HTTP 403). Vuelve a resolver el "
                "video; si persiste, actualiza yt-dlp y configura cookies de tu sesión "
                "o un proveedor de PO Token en los ajustes de YouTube.",
                retryable=True,
            )
        if 410 in statuses or re.search(r"\b410\b|url has expired|url expired", detail):
            return cls(
                "expired", "El enlace de reproducción caducó. Vuelve a resolver el video.",
                retryable=True,
            )
        if any(term in detail for term in (
            "video unavailable", "video is unavailable", "private video", "removed",
            "not available in your country", "not available in your region", "copyright",
            "members-only", "age-restricted", "requested format is not available",
        )) or 404 in statuses:
            return cls(
                "unavailable",
                "El video o formato no está disponible para esta sesión. Comprueba el "
                "enlace y, si tienes acceso al contenido, configura tus cookies de YouTube.",
            )
        if any(term in detail for term in (
            "timeout", "timed out", "connection", "network", "temporary failure",
            "name resolution", "remote disconnected", "urlerror",
        )) or any(500 <= status < 600 for status in statuses):
            return cls(
                "network",
                "La conexión con YouTube falló temporalmente. Comprueba la red y vuelve "
                "a intentar en unos momentos.",
                retryable=True,
            )
        return cls(
            "extraction",
            "No se pudo obtener el contenido de YouTube. Comprueba el enlace y actualiza "
            "el motor de descarga; revisa la configuración de acceso si el fallo persiste.",
        )


class _SafeLogger:
    """yt-dlp sometimes logs signed media URLs and provider credentials."""

    def debug(self, message: str) -> None:
        pass

    def warning(self, message: str) -> None:
        failure = YoutubeFailure.from_exception(message)
        logging.getLogger(__name__).warning("YouTube [%s]: %s", failure.code, failure.message)

    def error(self, message: str) -> None:
        failure = YoutubeFailure.from_exception(message)
        logging.getLogger(__name__).error("YouTube [%s]: %s", failure.code, failure.message)


class YoutubeAccessPolicy:
    """Build fresh options from live settings; environment values take precedence."""

    def __init__(
        self,
        config: AppConfig | None = None,
        settings: Mapping[str, Any] | Callable[[], Mapping[str, Any]] | None = None,
    ) -> None:
        self.config = config
        self.settings = settings

    def _preferences(self) -> Mapping[str, Any]:
        return self.settings() if callable(self.settings) else (self.settings or {})

    @staticmethod
    def _value(values: Mapping[str, Any], key: str, default: Any = "") -> Any:
        return os.environ.get(f"YTU_{key.upper()}", values.get(key, default))

    @staticmethod
    def _invalid(message: str) -> YoutubeFailure:
        return YoutubeFailure("configuration", message)

    @classmethod
    def _integer(cls, value: Any, minimum: int, maximum: int, key: str) -> int:
        try:
            number = int(value)
        except (ValueError, TypeError):
            raise cls._invalid(f"El ajuste {key} debe ser un número entero.") from None
        if isinstance(value, bool) or str(number) != str(value).strip():
            raise cls._invalid(f"El ajuste {key} debe ser un número entero.")
        if not minimum <= number <= maximum:
            raise cls._invalid(f"El ajuste {key} debe estar entre {minimum} y {maximum}.")
        return number

    def _runtime_candidates(self, executable: str) -> list[Path]:
        candidates: list[Path] = []
        if self.config is not None:
            candidates.append(self.config.resource_dir / "bin" / executable)
        if executable in {"node", "node.exe"}:
            try:
                spec = importlib.util.find_spec("nodejs_wheel")
            except (ImportError, ValueError):
                spec = None
            if spec and spec.origin:
                root = Path(spec.origin).parent
                candidates.extend((root / "bin" / executable, root / executable))
        candidates.append(Path(sys.executable).parent / executable)
        found = shutil.which(executable)
        if found:
            candidates.append(Path(found))
        return candidates

    def _runtimes(self) -> dict[str, dict[str, str]]:
        executables = {"deno": "deno", "node": "node", "quickjs": "qjs", "bun": "bun"}
        selected = os.getenv("YTU_JS_RUNTIME", "").strip().lower()
        explicit = os.getenv("YTU_JS_RUNTIME_PATH", "").strip()
        node_path = os.getenv("YTU_NODE_PATH", "").strip()
        if not selected and node_path and not explicit:
            selected, explicit = "node", node_path
        if selected and selected not in executables:
            raise self._invalid("YTU_JS_RUNTIME debe ser deno, node, quickjs o bun.")
        if explicit and not selected:
            raise self._invalid("Configura YTU_JS_RUNTIME junto con YTU_JS_RUNTIME_PATH.")
        if explicit:
            path = Path(explicit).expanduser()
            if not path.is_file() or not os.access(path, os.X_OK):
                raise self._invalid("La ruta configurada del runtime JavaScript no es ejecutable.")
            return {selected: {"path": str(path.resolve())}}
        result: dict[str, dict[str, str]] = {}
        for name, basename in executables.items():
            if selected and name != selected:
                continue
            executable = f"{basename}.exe" if os.name == "nt" else basename
            for path in self._runtime_candidates(executable):
                if path.is_file() and os.access(path, os.X_OK):
                    result[name] = {"path": str(path.resolve())}
                    break
        if selected and not result:
            raise self._invalid("No se encontró el runtime JavaScript seleccionado.")
        return result

    @classmethod
    def _browser(cls, value: Any) -> tuple[str | None, ...]:
        if isinstance(value, str):
            value = value.strip()
            if value.startswith("["):
                try:
                    value = json.loads(value)
                except ValueError:
                    raise cls._invalid("La configuración JSON del navegador no es válida.") from None
            else:
                match = re.fullmatch(
                    r"(?P<name>[^+:]+)(?:\+(?P<keyring>[^:]+))?"
                    r"(?::(?!:)(?P<profile>.+?))?(?:::(?P<container>.+))?", value,
                )
                if not match:
                    raise cls._invalid("La configuración de cookies del navegador no es válida.")
                value = [match["name"], match["profile"], match["keyring"], match["container"]]
        if not isinstance(value, (list, tuple)) or not 1 <= len(value) <= 4:
            raise cls._invalid("Configura el navegador o una lista JSON de hasta cuatro campos.")
        if not isinstance(value[0], str) or not value[0].strip():
            raise cls._invalid("Falta el nombre del navegador para las cookies.")
        if any(item is not None and not isinstance(item, str) for item in value):
            raise cls._invalid("Los campos del navegador deben ser texto o null.")
        fields = [*value, *([None] * (4 - len(value)))]
        fields[0] = fields[0].lower().strip()
        if fields[2]:
            fields[2] = fields[2].upper()
        return tuple(fields)

    @classmethod
    def _extractor_args(cls, value: Any) -> dict[str, dict[str, list[str]]]:
        if isinstance(value, str):
            try:
                value = json.loads(value)
            except ValueError:
                raise cls._invalid("youtube_extractor_args debe contener JSON válido.") from None
        if not isinstance(value, Mapping):
            raise cls._invalid("youtube_extractor_args debe ser un objeto JSON.")
        result: dict[str, dict[str, list[str]]] = {}
        for extractor, arguments in value.items():
            if not isinstance(extractor, str) or not isinstance(arguments, Mapping):
                raise cls._invalid("Cada extractor debe contener un objeto de argumentos.")
            result[extractor] = {}
            for key, items in arguments.items():
                if isinstance(items, str):
                    items = [items]
                if not isinstance(key, str) or not isinstance(items, (list, tuple)):
                    raise cls._invalid("Los argumentos del extractor deben ser listas de texto.")
                if not all(isinstance(item, str) for item in items):
                    raise cls._invalid("Los argumentos del extractor deben ser listas de texto.")
                result[extractor][key] = list(items)
        return result

    def options(self) -> dict[str, Any]:
        values = self._preferences()
        retries = self._integer(self._value(values, "download_retries", 2), 0, 10, "download_retries")
        timeout = self._integer(
            self._value(values, "download_socket_timeout", 20), 1, 120, "download_socket_timeout",
        )
        cache = self.config.cache_dir if self.config else Path(user_cache_path(APP_NAME, APP_AUTHOR))
        result: dict[str, Any] = {
            "quiet": True,
            "logger": _SafeLogger(),
            "cachedir": str(cache / "yt-dlp"),
            "js_runtimes": self._runtimes(),
            "socket_timeout": timeout,
            "retries": retries,
            "fragment_retries": retries,
            "extractor_retries": min(retries, 1),
            "file_access_retries": 1,
            "skip_unavailable_fragments": False,
        }
        cookie_file = self._value(values, "youtube_cookie_file")
        browser = self._value(values, "youtube_cookies_browser")
        if cookie_file and browser:
            raise self._invalid("Elige un archivo de cookies o un navegador, solo una fuente de sesión.")
        if cookie_file:
            path = Path(str(cookie_file)).expanduser()
            if not path.is_file() or not os.access(path, os.R_OK):
                raise self._invalid("No se puede leer el archivo de cookies configurado para YouTube.")
            result["cookiefile"] = str(path.resolve())
        if browser:
            result["cookiesfrombrowser"] = self._browser(browser)
        extractor_args = self._value(values, "youtube_extractor_args")
        if extractor_args:
            result["extractor_args"] = self._extractor_args(extractor_args)
        proxy = self._value(values, "youtube_proxy")
        if proxy:
            try:
                parsed = urlsplit(str(proxy))
                valid = parsed.scheme in {"http", "https", "socks4", "socks4a", "socks5", "socks5h"}
                valid = valid and bool(parsed.hostname) and not parsed.query and not parsed.fragment
                valid = valid and parsed.path in {"", "/"}
                if parsed.port is not None:
                    valid = valid and 1 <= parsed.port <= 65535
            except ValueError:
                valid = False
            if not valid:
                raise self._invalid("youtube_proxy debe ser una URL HTTP, HTTPS o SOCKS válida.")
            result["proxy"] = str(proxy)
        return result

    def fingerprint(self) -> str:
        """Partition playback caches when configuration or cookie files change."""
        options = self.options()
        options.pop("logger", None)
        cookie_file = options.get("cookiefile")
        if cookie_file:
            try:
                stat = Path(cookie_file).stat()
                options["cookie_revision"] = (stat.st_mtime_ns, stat.st_size, stat.st_ino)
            except OSError:
                options["cookie_revision"] = None
        payload = json.dumps(options, sort_keys=True, separators=(",", ":"))
        return hashlib.sha256(payload.encode("utf-8")).hexdigest()

    def diagnostics(self) -> dict[str, Any]:
        """Report availability without publishing credentials or local user paths."""
        versions: dict[str, str | None] = {}
        for package in ("yt-dlp", "yt-dlp-ejs", "pytubefix"):
            try:
                versions[package] = importlib.metadata.version(package)
            except importlib.metadata.PackageNotFoundError:
                versions[package] = None
        try:
            options = self.options()
        except YoutubeFailure as failure:
            return {
                "versions": versions,
                "js_runtimes": [],
                "authentication": [],
                "extractor_args_configured": False,
                "proxy_configured": False,
                "configuration_error": {"code": failure.code, "message": failure.message},
            }
        authentication = []
        if "cookiefile" in options:
            authentication.append("cookie_file")
        if "cookiesfrombrowser" in options:
            authentication.append("browser")
        return {
            "versions": versions,
            "js_runtimes": list(options["js_runtimes"]),
            "authentication": authentication or ["anonymous"],
            "extractor_args_configured": bool(options.get("extractor_args")),
            "proxy_configured": "proxy" in options,
        }
