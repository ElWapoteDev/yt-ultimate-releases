from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Any, Iterable, Mapping, Protocol, Sequence
from urllib.parse import parse_qs, urlencode, urlparse, urlunparse


class SearchError(RuntimeError):
    """Base error for remote catalog search."""


class InvalidYouTubeURLError(SearchError, ValueError):
    """Raised when input is not a supported YouTube URL."""


class SearchUnavailableError(SearchError):
    """Raised when every configured search adapter fails."""


class SearchAdapter(Protocol):
    name: str

    def search(self, query: str, limit: int) -> Sequence[Mapping[str, Any]]: ...


_VIDEO_ID = re.compile(r"^[A-Za-z0-9_-]{11}$")
_PLAYLIST_ID = re.compile(r"^[A-Za-z0-9_-]{10,100}$")
_YOUTUBE_HOSTS = {
    "youtube.com",
    "www.youtube.com",
    "m.youtube.com",
    "music.youtube.com",
    "youtu.be",
    "www.youtu.be",
    "youtube-nocookie.com",
    "www.youtube-nocookie.com",
}


@dataclass(frozen=True, slots=True)
class SearchResult:
    id: str
    url: str
    title: str
    thumbnail: str = ""
    duration_seconds: int = 0
    channel: str = ""
    views: str = ""
    view_count: int | None = None

    @property
    def duration(self) -> str:
        return format_duration(self.duration_seconds)

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "url": self.url,
            "title": self.title,
            "thumbnail": self.thumbnail,
            "duration": self.duration,
            "duration_seconds": self.duration_seconds,
            "channel": self.channel,
            "views": self.views,
            "view_count": self.view_count,
        }

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> "SearchResult":
        raw_url = str(value.get("webpage_url") or value.get("url") or "")
        identifier = str(value.get("id") or extract_video_id(raw_url) or "")
        if not _VIDEO_ID.fullmatch(identifier):
            raise ValueError("Search result has no valid YouTube video id")
        duration = value.get("duration_seconds", value.get("duration", 0))
        duration_seconds = parse_duration(duration)
        view_count = _optional_int(value.get("view_count"))
        views = str(value.get("views") or "")
        if not views and view_count is not None:
            views = format_views(view_count)
        thumbnails = value.get("thumbnails") or []
        thumbnail = str(value.get("thumbnail") or "")
        if not thumbnail and isinstance(thumbnails, Sequence) and thumbnails:
            last = thumbnails[-1]
            if isinstance(last, Mapping):
                thumbnail = str(last.get("url") or "")
        if not thumbnail:
            thumbnail = f"https://i.ytimg.com/vi/{identifier}/hqdefault.jpg"
        return cls(
            id=identifier,
            url=f"https://www.youtube.com/watch?v={identifier}",
            title=str(value.get("title") or "Untitled"),
            thumbnail=thumbnail,
            duration_seconds=duration_seconds,
            channel=str(
                value.get("channel")
                or value.get("uploader")
                or value.get("author")
                or ""
            ),
            views=views,
            view_count=view_count,
        )


def format_duration(seconds: int | float | None) -> str:
    try:
        total = max(0, int(seconds or 0))
    except (TypeError, ValueError):
        return ""
    if total <= 0:
        return ""
    hours, remainder = divmod(total, 3600)
    minutes, secs = divmod(remainder, 60)
    if hours:
        return f"{hours}:{minutes:02d}:{secs:02d}"
    return f"{minutes}:{secs:02d}"


def parse_duration(value: Any) -> int:
    if isinstance(value, str) and ":" in value:
        try:
            parts = [int(part) for part in value.split(":")]
        except ValueError:
            return 0
        if not 1 <= len(parts) <= 3 or any(part < 0 for part in parts):
            return 0
        total = 0
        for part in parts:
            total = total * 60 + part
        return total
    try:
        return max(0, int(float(value or 0)))
    except (TypeError, ValueError):
        return 0


def format_views(count: int) -> str:
    if count >= 1_000_000_000:
        return f"{count / 1_000_000_000:.1f}B views"
    if count >= 1_000_000:
        return f"{count / 1_000_000:.1f}M views"
    if count >= 1_000:
        return f"{count / 1_000:.0f}K views"
    return f"{count} views"


def validate_youtube_url(value: str, *, allow_playlist: bool = True) -> str:
    """Validate and canonicalize a YouTube video or playlist URL."""

    raw = (value or "").strip()
    if not raw or len(raw) > 2048 or any(ord(char) < 32 for char in raw):
        raise InvalidYouTubeURLError("A valid YouTube URL is required")
    if _VIDEO_ID.fullmatch(raw):
        return f"https://www.youtube.com/watch?v={raw}"
    if "://" not in raw:
        raw = f"https://{raw}"

    parsed = urlparse(raw)
    host = (parsed.hostname or "").lower().rstrip(".")
    if parsed.scheme not in {"http", "https"} or host not in _YOUTUBE_HOSTS:
        raise InvalidYouTubeURLError("Only YouTube URLs are supported")
    if parsed.username or parsed.password:
        raise InvalidYouTubeURLError("Credentials are not allowed in YouTube URLs")
    try:
        port = parsed.port
    except ValueError as exc:
        raise InvalidYouTubeURLError("Invalid URL port") from exc
    if port not in {None, 80, 443}:
        raise InvalidYouTubeURLError("Unexpected port in YouTube URL")

    query = parse_qs(parsed.query)
    video_id = ""
    if host in {"youtu.be", "www.youtu.be"}:
        video_id = parsed.path.strip("/").split("/", 1)[0]
    elif parsed.path.rstrip("/") == "/watch":
        video_id = (query.get("v") or [""])[0]
    else:
        path_parts = [part for part in parsed.path.split("/") if part]
        if (
            len(path_parts) >= 2
            and path_parts[0] in {"shorts", "embed", "live", "v"}
        ):
            video_id = path_parts[1]

    playlist_id = (query.get("list") or [""])[0]
    if video_id and _VIDEO_ID.fullmatch(video_id):
        canonical_query = {"v": video_id}
        if allow_playlist and playlist_id and _PLAYLIST_ID.fullmatch(playlist_id):
            canonical_query["list"] = playlist_id
        return urlunparse(
            ("https", "www.youtube.com", "/watch", "", urlencode(canonical_query), "")
        )
    if allow_playlist and playlist_id and _PLAYLIST_ID.fullmatch(playlist_id):
        return f"https://www.youtube.com/playlist?{urlencode({'list': playlist_id})}"
    raise InvalidYouTubeURLError("The URL has no valid YouTube video or playlist id")


def normalize_youtube_url(value: str) -> str:
    return validate_youtube_url(value, allow_playlist=True)


def extract_video_id(value: str) -> str | None:
    try:
        normalized = validate_youtube_url(value, allow_playlist=False)
    except InvalidYouTubeURLError:
        return value if _VIDEO_ID.fullmatch(value or "") else None
    identifier = (parse_qs(urlparse(normalized).query).get("v") or [None])[0]
    return identifier if identifier and _VIDEO_ID.fullmatch(identifier) else None


def extract_playlist_id(value: str) -> str | None:
    try:
        normalized = validate_youtube_url(value, allow_playlist=True)
    except InvalidYouTubeURLError:
        return None
    identifier = (parse_qs(urlparse(normalized).query).get("list") or [None])[0]
    return identifier if identifier and _PLAYLIST_ID.fullmatch(identifier) else None


def normalize_playlist_url(value: str) -> str:
    normalized = validate_youtube_url(value, allow_playlist=True)
    query = parse_qs(urlparse(normalized).query)
    identifier = (query.get("list") or [None])[0]
    if not identifier:
        raise InvalidYouTubeURLError("The URL has no valid YouTube playlist id")
    video_id = (query.get("v") or [None])[0]
    if (
        identifier.startswith(("RD", "OL"))
        and video_id
        and _VIDEO_ID.fullmatch(video_id)
    ):
        return (
            "https://www.youtube.com/watch?"
            f"{urlencode({'v': video_id, 'list': identifier})}"
        )
    return f"https://www.youtube.com/playlist?{urlencode({'list': identifier})}"


class YtDlpSearchAdapter:
    name = "yt-dlp"

    def __init__(self, module: Any | None = None) -> None:
        if module is None:
            import yt_dlp as module  # type: ignore[no-redef]

        self._module = module

    def search(self, query: str, limit: int) -> Sequence[Mapping[str, Any]]:
        options = {
            "quiet": True,
            "no_warnings": True,
            "extract_flat": "in_playlist",
            "default_search": "ytsearch",
            "skip_download": True,
        }
        with self._module.YoutubeDL(options) as downloader:
            payload = downloader.extract_info(
                f"ytsearch{limit}:{query}",
                download=False,
            )
        return [
            entry
            for entry in (payload or {}).get("entries", [])
            if isinstance(entry, Mapping)
        ]


class PytubeSearchAdapter:
    name = "pytubefix"

    def __init__(self, search_class: Any | None = None) -> None:
        if search_class is None:
            from pytubefix import Search as search_class  # type: ignore[no-redef]

        self._search_class = search_class

    def search(self, query: str, limit: int) -> Sequence[Mapping[str, Any]]:
        search = self._search_class(query)
        results: list[dict[str, Any]] = []
        for video in list(search.videos)[:limit]:
            results.append(
                {
                    "id": getattr(video, "video_id", ""),
                    "title": getattr(video, "title", ""),
                    "thumbnail": getattr(video, "thumbnail_url", ""),
                    "duration": getattr(video, "length", 0),
                    "channel": getattr(video, "author", ""),
                    "view_count": getattr(video, "views", None),
                }
            )
        return results


class SearchService:
    def __init__(
        self,
        adapters: Iterable[SearchAdapter] | None = None,
        *,
        max_results: int = 50,
        strict: bool = False,
    ) -> None:
        if max_results < 1:
            raise ValueError("max_results must be positive")
        self.max_results = max_results
        self.strict = strict
        self.adapters = tuple(adapters) if adapters is not None else self._defaults()

    @staticmethod
    def _defaults() -> tuple[SearchAdapter, ...]:
        adapters: list[SearchAdapter] = []
        try:
            adapters.append(YtDlpSearchAdapter())
        except ImportError:
            pass
        try:
            adapters.append(PytubeSearchAdapter())
        except ImportError:
            pass
        return tuple(adapters)

    def search(self, query: str, limit: int = 20) -> list[dict[str, Any]]:
        normalized_query = _validate_query(query)
        if isinstance(limit, bool) or not isinstance(limit, int):
            raise TypeError("limit must be an integer")
        if not 1 <= limit <= self.max_results:
            raise ValueError(f"limit must be between 1 and {self.max_results}")
        errors: list[str] = []
        for adapter in self.adapters:
            try:
                values = adapter.search(normalized_query, limit)
                normalized = self._normalize(values, limit)
                if normalized:
                    return [result.to_dict() for result in normalized]
            except Exception as exc:
                errors.append(f"{getattr(adapter, 'name', type(adapter).__name__)}: {exc}")
        if self.strict and errors:
            raise SearchUnavailableError(" | ".join(errors))
        return []

    @staticmethod
    def _normalize(
        values: Iterable[Mapping[str, Any]],
        limit: int,
    ) -> list[SearchResult]:
        results: list[SearchResult] = []
        seen: set[str] = set()
        for value in values:
            try:
                result = SearchResult.from_mapping(value)
            except (TypeError, ValueError):
                continue
            if result.id in seen:
                continue
            seen.add(result.id)
            results.append(result)
            if len(results) >= limit:
                break
        return results


def _validate_query(query: str) -> str:
    value = (query or "").strip()
    if not value:
        raise ValueError("Search query cannot be empty")
    if len(value) > 300:
        raise ValueError("Search query is too long")
    if any(ord(char) < 32 and char not in {"\t", "\n"} for char in value):
        raise ValueError("Search query contains control characters")
    return " ".join(value.split())


def _optional_int(value: Any) -> int | None:
    try:
        converted = int(value)
    except (TypeError, ValueError):
        return None
    return converted if converted >= 0 else None
