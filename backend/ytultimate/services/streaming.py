from __future__ import annotations

import ipaddress
import math
import threading
import time
from collections import OrderedDict
from concurrent.futures import Future
from dataclasses import dataclass
from typing import Any, Callable, Mapping, Protocol, Sequence
from urllib.parse import parse_qs, urljoin, urlparse

from starlette.responses import Response, StreamingResponse

from ytultimate.services.search import validate_youtube_url
from ytultimate.services.youtube import YoutubeAccessPolicy, YoutubeFailure


class StreamingError(RuntimeError):
    """Base error for remote media preview."""


class StreamExtractor(Protocol):
    def extract(self, youtube_url: str) -> Mapping[str, Any]: ...


@dataclass(frozen=True, slots=True)
class StreamInfo:
    source_url: str
    audio_url: str
    title: str
    channel: str
    duration: float
    thumbnail: str
    mime_type: str
    request_headers: Mapping[str, str]
    extracted_at: float

    def to_dict(self) -> dict[str, Any]:
        return {
            "source_url": self.source_url,
            "audio_url": self.audio_url,
            "title": self.title,
            "channel": self.channel,
            "duration": self.duration,
            "thumbnail": self.thumbnail,
            "mime_type": self.mime_type,
        }


@dataclass(frozen=True, slots=True)
class _CacheEntry:
    value: StreamInfo
    expires_at: float


class YtDlpStreamExtractor:
    # The official extractor resolves media on these CDN domains. Custom
    # extractors may declare their own allowed_stream_hosts on the same contract.
    allowed_stream_hosts = ("googlevideo.com", "youtube.com")

    def __init__(
        self,
        module: Any | None = None,
        *,
        access_policy: YoutubeAccessPolicy | None = None,
    ) -> None:
        if module is None:
            import yt_dlp as module  # type: ignore[no-redef]

        self._module = module
        self.access_policy = access_policy or YoutubeAccessPolicy()

    def extract(self, youtube_url: str) -> Mapping[str, Any]:
        options = {
            **self.access_policy.options(),
            # A seekable HTTP resource is required by the audio proxy; manifests
            # and segmented protocols cannot be relabeled as audio bytes.
            "format": (
                "bestaudio[ext=m4a][protocol=https]/"
                "bestaudio[ext=webm][protocol=https]/"
                "bestaudio[protocol=https]/bestaudio[protocol=http]"
            ),
            "skip_download": True,
            "noplaylist": True,
        }
        try:
            with self._module.YoutubeDL(options) as downloader:
                payload = downloader.extract_info(youtube_url, download=False) or {}
                candidates = [payload, *(payload.get("formats") or [])]
                playable = [
                    item for item in candidates
                    if isinstance(item, Mapping) and _is_direct_audio(item)
                ]
                if not playable:
                    raise StreamingError(
                        "YouTube no devolvió audio HTTP reproducible. "
                        "Revisa el diagnóstico de acceso a YouTube en ajustes."
                    )
                selected = max(playable, key=_audio_preference)
                audio_url = str(selected["url"])
                headers = {
                    **dict(payload.get("http_headers") or {}),
                    **dict(selected.get("http_headers") or {}),
                }
                # yt-dlp removes unscoped Cookie headers from info dictionaries.
                # Ask its jar for cookies scoped specifically to this media URL.
                cookiejar = getattr(downloader, "cookiejar", None)
                cookie_header = getattr(cookiejar, "get_cookie_header", None)
                if callable(cookie_header):
                    cookie = cookie_header(audio_url)
                    if cookie:
                        headers["Cookie"] = cookie
                return {
                    "audio_url": audio_url,
                    "title": payload.get("title") or "",
                    "channel": payload.get("channel") or payload.get("uploader") or "",
                    "duration": payload.get("duration") or 0,
                    "thumbnail": payload.get("thumbnail") or "",
                    "mime_type": _format_mime_type(selected),
                    "http_headers": headers,
                }
        except StreamingError:
            raise
        except Exception as exc:
            raise StreamingError(YoutubeFailure.from_exception(exc).message) from None


class _ThreadLocalSessions:
    """Reuse connections without sharing requests' mutable cookie jars between threads."""

    def __init__(self) -> None:
        self._local = threading.local()
        self._lock = threading.Lock()
        self._sessions: list[Any] = []
        self._closed = False

    def request(self, *args: Any, **kwargs: Any) -> Any:
        import requests
        from requests.adapters import HTTPAdapter

        with self._lock:
            if self._closed:
                raise StreamingError("El servicio de streaming está cerrado")
            session = getattr(self._local, "session", None)
            if session is None:
                session = requests.Session()
                adapter = HTTPAdapter(pool_connections=8, pool_maxsize=16, max_retries=0)
                session.mount("http://", adapter)
                session.mount("https://", adapter)
                self._local.session = session
                self._sessions.append(session)
        return session.request(*args, **kwargs)

    def close(self) -> None:
        with self._lock:
            self._closed = True
            sessions, self._sessions = self._sessions, []
        for session in sessions:
            session.close()


class _ClosingStreamingResponse(StreamingResponse):
    """Close even if ASGI disconnects before the body generator is first entered."""

    def __init__(self, content: Any, *, close: Callable[[], None], **kwargs: Any) -> None:
        super().__init__(content, **kwargs)
        self._close_upstream = close

    async def __call__(self, scope: Any, receive: Any, send: Any) -> None:
        try:
            await super().__call__(scope, receive, send)
        finally:
            self._close_upstream()


class StreamingService:
    """Resolve scoped, expiring URLs once per source and proxy bounded HTTP streams."""

    def __init__(
        self,
        *,
        extractor: StreamExtractor | None = None,
        session: Any | None = None,
        access_policy: YoutubeAccessPolicy | None = None,
        cache_ttl: float = 600,
        max_cache_entries: int = 128,
        clock: Callable[[], float] = time.time,
        chunk_size: int = 64 * 1024,
        connect_timeout: float = 10,
        read_timeout: float = 30,
        user_agent: str = "YT Ultimate/4.0",
        max_refresh_attempts: int = 1,
        max_redirects: int = 4,
        allowed_stream_hosts: Sequence[str] | None = None,
    ) -> None:
        if not math.isfinite(cache_ttl) or cache_ttl <= 0:
            raise ValueError("cache_ttl must be positive and finite")
        if max_cache_entries < 1:
            raise ValueError("max_cache_entries must be positive")
        if chunk_size < 1024:
            raise ValueError("chunk_size must be at least 1024 bytes")
        if any(not math.isfinite(value) or value <= 0 for value in (connect_timeout, read_timeout)):
            raise ValueError("Network timeouts must be positive and finite")
        if max_refresh_attempts < 0 or max_redirects < 0:
            raise ValueError("Retry and redirect limits must be nonnegative")
        self.access_policy = access_policy or getattr(extractor, "access_policy", None)
        if extractor is None:
            self.access_policy = self.access_policy or YoutubeAccessPolicy()
            extractor = YtDlpStreamExtractor(access_policy=self.access_policy)
        self.extractor = extractor
        self.session = session if session is not None else _ThreadLocalSessions()
        self.cache_ttl = float(cache_ttl)
        self.max_cache_entries = max_cache_entries
        self.clock = clock
        self.chunk_size = chunk_size
        self.timeout = (connect_timeout, read_timeout)
        self.user_agent = user_agent
        self.max_refresh_attempts = max_refresh_attempts
        self.max_redirects = max_redirects
        hosts = allowed_stream_hosts
        if hosts is None:
            hosts = getattr(extractor, "allowed_stream_hosts", None)
        self.allowed_stream_hosts = tuple(hosts) if hosts is not None else None
        self._cache: OrderedDict[tuple[str, str], _CacheEntry] = OrderedDict()
        self._inflight: dict[tuple[str, str], Future[StreamInfo]] = {}
        self._active: dict[int, Callable[[], None]] = {}
        self._lock = threading.RLock()
        self._closed = False

    def resolve(self, url: str, *, refresh: bool = False) -> StreamInfo:
        return self._resolve(url, refresh=refresh)

    def _resolve(
        self, url: str, *, refresh: bool = False, previous: StreamInfo | None = None,
    ) -> StreamInfo:
        canonical = validate_youtube_url(url, allow_playlist=False)
        scope = self.access_policy.fingerprint() if self.access_policy is not None else ""
        key = (scope, canonical)
        with self._lock:
            if self._closed:
                raise StreamingError("El servicio de streaming está cerrado")
            now = self.clock()
            self._purge_locked(now)
            cached = self._cache.get(key)
            if cached is not None and (not refresh or (previous is not None and cached.value is not previous)):
                self._cache.move_to_end(key)
                return cached.value
            pending = self._inflight.get(key)
            owner = pending is None
            if owner:
                pending = Future()
                self._inflight[key] = pending
        assert pending is not None
        if not owner:
            return pending.result()
        try:
            payload = self.extractor.extract(canonical)
            now = self.clock()
            audio_url = str(payload.get("audio_url") or payload.get("url") or "")
            _validate_remote_stream_url(audio_url, allowed_hosts=self.allowed_stream_hosts)
            mime_type = str(payload.get("mime_type") or "audio/mp4")
            if not _playable_content_type(mime_type):
                raise StreamingError("El extractor devolvió un manifiesto o contenido sin audio")
            value = StreamInfo(
                source_url=canonical,
                audio_url=audio_url,
                title=str(payload.get("title") or ""),
                channel=str(payload.get("channel") or payload.get("uploader") or ""),
                duration=_number(payload.get("duration")),
                thumbnail=str(payload.get("thumbnail") or ""),
                mime_type=mime_type,
                request_headers=_sanitize_extractor_headers(payload.get("http_headers")),
                extracted_at=now,
            )
            expires_at = self._cache_expiration(audio_url, now)
            with self._lock:
                if not self._closed and expires_at > now:
                    self._cache[key] = _CacheEntry(value=value, expires_at=expires_at)
                    self._cache.move_to_end(key)
                    self._purge_locked(now)
            pending.set_result(value)
            return value
        except BaseException as exc:
            pending.set_exception(exc)
            raise
        finally:
            with self._lock:
                self._inflight.pop(key, None)

    def get_stream(self, url: str, *, refresh: bool = False) -> dict[str, Any]:
        return self.resolve(url, refresh=refresh).to_dict()

    def build_proxy_response(
        self,
        url: str,
        request_headers: Mapping[str, str] | None = None,
        *,
        method: str = "GET",
        refresh: bool = False,
    ) -> Any:
        """Preserve range/HEAD semantics and renew URL plus credentials on stale access."""
        normalized_method = method.upper()
        if normalized_method not in {"GET", "HEAD"}:
            raise ValueError("Streaming proxy only supports GET and HEAD")
        source_headers = {
            str(key).lower(): str(value) for key, value in (request_headers or {}).items()
        }
        byte_range = source_headers.get("range") if normalized_method == "GET" else None
        if byte_range and not _valid_range_header(byte_range):
            raise ValueError("Invalid Range header")
        stream = self.resolve(url, refresh=refresh)
        upstream = None
        for attempt in range(self.max_refresh_attempts + 1):
            upstream = self._request_upstream(stream, normalized_method, source_headers)
            status_code = int(getattr(upstream, "status_code", 0))
            if status_code not in {401, 403, 410} or attempt == self.max_refresh_attempts:
                break
            upstream.close()
            stream = self._resolve(url, refresh=True, previous=stream)
        assert upstream is not None
        status_code = int(getattr(upstream, "status_code", 0))
        if status_code not in {200, 206, 416}:
            upstream.close()
            if status_code == 429:
                raise StreamingError("YouTube limitó las solicitudes. Espera antes de volver a reproducir.")
            if status_code in {401, 403, 410}:
                raise StreamingError(
                    "YouTube rechazó el audio tras renovar el enlace. "
                    "Revisa el diagnóstico de acceso y la sesión en ajustes."
                )
            raise StreamingError(f"Upstream media server returned HTTP {status_code}")
        try:
            response_headers = _proxy_response_headers(
                getattr(upstream, "headers", {}), fallback_mime=stream.mime_type,
            )
            if status_code != 416 and not _playable_content_type(response_headers["Content-Type"]):
                raise StreamingError("El servidor devolvió un manifiesto o contenido sin audio")
            if normalized_method == "HEAD" or status_code == 416:
                if status_code == 416:
                    # The upstream's error body is deliberately not forwarded.
                    response_headers["Content-Length"] = "0"
                return Response(content=b"", status_code=status_code, headers=response_headers)
            close = self._register_upstream(upstream)

            def body_iterator() -> Any:
                try:
                    for chunk in upstream.iter_content(chunk_size=self.chunk_size):
                        if chunk:
                            yield chunk
                finally:
                    close()

            return _ClosingStreamingResponse(
                body_iterator(), close=close, status_code=status_code,
                media_type=None, headers=response_headers,
            )
        except BaseException:
            upstream.close()
            raise
        finally:
            if normalized_method == "HEAD" or status_code == 416:
                upstream.close()

    proxy_response = build_proxy_response

    def _request_upstream(
        self, stream: StreamInfo, method: str, source_headers: Mapping[str, str],
    ) -> Any:
        headers = {
            **dict(stream.request_headers),
            "User-Agent": stream.request_headers.get("User-Agent", self.user_agent),
            "Accept": "*/*",
            "Accept-Encoding": "identity",
        }
        if method == "GET" and source_headers.get("range"):
            headers["Range"] = source_headers["range"]
            if_range = source_headers.get("if-range", "")
            if if_range and _safe_header_value(if_range, limit=512):
                headers["If-Range"] = if_range
        kwargs: dict[str, Any] = {}
        if self.access_policy is not None:
            proxy = self.access_policy.options().get("proxy")
            if proxy is not None:
                kwargs["proxies"] = {"http": proxy, "https": proxy}
        current_url = stream.audio_url
        for redirect in range(self.max_redirects + 1):
            _validate_remote_stream_url(current_url, allowed_hosts=self.allowed_stream_hosts)
            try:
                request_method = getattr(self.session, "request", None)
                options = {
                    "headers": headers, "stream": method == "GET", "timeout": self.timeout,
                    "allow_redirects": False, **kwargs,
                }
                if callable(request_method):
                    response = request_method(method, current_url, **options)
                else:
                    requester = getattr(self.session, "head" if method == "HEAD" else "get")
                    response = requester(current_url, **options)
            except Exception as exc:
                raise StreamingError(YoutubeFailure.from_exception(exc).message) from None
            status = int(getattr(response, "status_code", 0))
            if status not in {301, 302, 303, 307, 308}:
                return response
            location = _lower_headers(getattr(response, "headers", {})).get("location")
            response.close()
            if not location or redirect == self.max_redirects:
                raise StreamingError("El servidor de audio excedió el límite de redirecciones")
            next_url = urljoin(current_url, location)
            _validate_remote_stream_url(next_url, allowed_hosts=self.allowed_stream_hosts)
            if _origin(next_url) != _origin(current_url):
                headers = {key: value for key, value in headers.items() if key != "Cookie"}
            current_url = next_url
        raise AssertionError("Unreachable redirect state")

    def _register_upstream(self, upstream: Any) -> Callable[[], None]:
        closed = False

        def close() -> None:
            nonlocal closed
            with self._lock:
                if closed:
                    return
                closed = True
                self._active.pop(id(upstream), None)
            upstream.close()

        with self._lock:
            if self._closed:
                upstream.close()
                raise StreamingError("El servicio de streaming está cerrado")
            self._active[id(upstream)] = close
        return close

    def close(self) -> None:
        with self._lock:
            self._closed = True
            self._cache.clear()
            closers = tuple(self._active.values())
        try:
            for close in closers:
                close()
        finally:
            close_session = getattr(self.session, "close", None)
            if callable(close_session):
                close_session()

    def clear_cache(self) -> int:
        with self._lock:
            size = len(self._cache)
            self._cache.clear()
            return size

    def cache_info(self) -> Mapping[str, Any]:
        with self._lock:
            self._purge_locked(self.clock())
            return {"size": len(self._cache), "max_size": self.max_cache_entries, "ttl_seconds": self.cache_ttl}

    def _cache_expiration(self, stream_url: str, now: float) -> float:
        expires_at = now + self.cache_ttl
        raw_expire = (parse_qs(urlparse(stream_url).query).get("expire") or [None])[0]
        if raw_expire is not None:
            try:
                signed_expiration = float(raw_expire)
            except (TypeError, ValueError):
                return expires_at
            if math.isfinite(signed_expiration):
                if signed_expiration <= now:
                    raise StreamingError("YouTube devolvió un enlace de audio caducado")
                expires_at = min(expires_at, signed_expiration - 30)
        return expires_at

    def _purge_locked(self, now: float) -> None:
        for key in [key for key, entry in self._cache.items() if entry.expires_at <= now]:
            self._cache.pop(key, None)
        while len(self._cache) > self.max_cache_entries:
            self._cache.popitem(last=False)


def _is_direct_audio(value: Mapping[str, Any]) -> bool:
    url = str(value.get("url") or "")
    parsed = urlparse(url)
    protocol = str(value.get("protocol") or parsed.scheme)
    return (
        parsed.scheme in {"https", "http"}
        and protocol in {"https", "http"}
        and value.get("acodec") not in {None, "none"}
        and value.get("vcodec") in {None, "none"}
        and not value.get("has_drm")
        and not value.get("fragments")
        and not parsed.path.lower().endswith((".m3u8", ".mpd"))
    )


def _audio_preference(value: Mapping[str, Any]) -> tuple[int, float, float]:
    extension = str(value.get("ext") or "").lower()
    return (2 if extension == "m4a" else 1 if extension == "webm" else 0,
            _number(value.get("abr")), _number(value.get("tbr")))


def _format_mime_type(value: Mapping[str, Any]) -> str:
    extension = str(value.get("ext") or "").lower()
    if extension in {"webm", "opus", "ogg", "mp3", "aac", "flac"}:
        return {"webm": "audio/webm", "opus": "audio/ogg", "ogg": "audio/ogg",
                "mp3": "audio/mpeg", "aac": "audio/aac", "flac": "audio/flac"}[extension]
    return "audio/mp4"


def _validate_remote_stream_url(
    value: str, *, allowed_hosts: Sequence[str] | None = None,
) -> None:
    if not value or len(value) > 16384 or any(ord(char) < 33 for char in value):
        raise StreamingError("Extractor returned an invalid stream URL")
    try:
        parsed = urlparse(value)
        host = (parsed.hostname or "").lower().rstrip(".")
        port = parsed.port
    except ValueError:
        raise StreamingError("Extractor returned an invalid stream URL") from None
    if parsed.scheme not in {"http", "https"} or not host:
        raise StreamingError("Extractor returned a non-HTTP stream URL")
    if parsed.username or parsed.password:
        raise StreamingError("Stream URL cannot contain credentials")
    if port not in {None, 80, 443}:
        raise StreamingError("Stream URL uses an unsupported port")
    if host == "localhost" or host.endswith((".localhost", ".local", ".internal")):
        raise StreamingError("Stream URL cannot target a local host")
    try:
        address = ipaddress.ip_address(host)
    except ValueError:
        address = None
    if address is not None and not address.is_global:
        raise StreamingError("Stream URL cannot target a private address")
    if allowed_hosts is not None and not any(
        host == domain.lower().rstrip(".") or host.endswith("." + domain.lower().rstrip("."))
        for domain in allowed_hosts
    ):
        raise StreamingError("Stream URL is outside the configured media hosts")
    if parsed.path.lower().endswith((".m3u8", ".mpd")):
        raise StreamingError("A media manifest cannot be proxied as an audio file")


def _origin(value: str) -> tuple[str, str | None, int | None]:
    parsed = urlparse(value)
    return parsed.scheme, parsed.hostname, parsed.port


def _valid_range_header(value: str) -> bool:
    if len(value) > 128 or "\r" in value or "\n" in value or not value.startswith("bytes="):
        return False
    ranges = value[6:].split(",")
    if not 1 <= len(ranges) <= 8:
        return False
    for item in ranges:
        start, separator, end = item.strip().partition("-")
        if not separator or (not start and not end):
            return False
        if (start and not (start.isascii() and start.isdigit())) or (end and not (end.isascii() and end.isdigit())):
            return False
        if (start and end and int(start) > int(end)) or (not start and int(end) == 0):
            return False
    return True


def _safe_header_value(value: str, *, limit: int = 8192) -> bool:
    return len(value) <= limit and all(ord(char) >= 32 and ord(char) != 127 for char in value)


def _lower_headers(value: Mapping[str, Any]) -> dict[str, str]:
    return {str(key).lower(): str(item) for key, item in value.items() if _safe_header_value(str(item))}


def _playable_content_type(value: str) -> bool:
    mime = value.split(";", 1)[0].strip().lower()
    return (mime.startswith("audio/") and "mpegurl" not in mime) or mime in {
        "video/mp4", "video/webm", "application/octet-stream", "binary/octet-stream", "multipart/byteranges",
    }


def _proxy_response_headers(
    upstream_headers: Mapping[str, Any], *, fallback_mime: str,
) -> dict[str, str]:
    lowered = _lower_headers(upstream_headers)
    if lowered.get("content-encoding", "identity").lower() != "identity":
        raise StreamingError("El servidor devolvió audio comprimido incompatible con rangos de bytes")
    output = {
        "Content-Type": lowered.get("content-type", fallback_mime),
        "Accept-Ranges": lowered.get("accept-ranges", "bytes" if "content-range" in lowered else "none"),
        "Cache-Control": "no-store",
        "X-Content-Type-Options": "nosniff",
    }
    names = {"content-length": "Content-Length", "content-range": "Content-Range", "etag": "ETag", "last-modified": "Last-Modified"}
    for source, destination in names.items():
        value = lowered.get(source)
        if value:
            if source == "content-length" and not (value.isascii() and value.isdigit()):
                raise StreamingError("El servidor devolvió una longitud de audio inválida")
            output[destination] = value
    return output


def _sanitize_extractor_headers(value: Any) -> dict[str, str]:
    if not isinstance(value, Mapping):
        return {}
    allowed = {"accept-language": "Accept-Language", "origin": "Origin", "referer": "Referer", "user-agent": "User-Agent", "cookie": "Cookie"}
    return {
        allowed[str(name).lower()]: str(item) for name, item in value.items()
        if str(name).lower() in allowed and _safe_header_value(str(item))
    }


def _number(value: Any) -> float:
    try:
        number = float(value or 0)
        return max(0.0, number) if math.isfinite(number) else 0.0
    except (TypeError, ValueError):
        return 0.0
