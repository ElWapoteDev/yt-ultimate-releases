use crate::{CancellationToken, MediaError, Result};
use innertube_rs::{
    endpoints::player::{resolve_stream_url_full, select_format},
    endpoints::playlist::{parse_playlist_browse_response, parse_playlist_continuation_response},
    models::playlist::PlaylistVideoItem,
    models::video::{GetVideoInfoOptions, VideoDetails},
    FormatFilter, FormatType, Innertube, QualityPreference, SearchResultItem,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::sync::OnceCell;
use url::Url;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: String,
    pub url: String,
    pub title: String,
    pub thumbnail: String,
    pub duration_seconds: u64,
    pub channel: String,
    pub view_count: u64,
}

pub const PLAYLIST_ITEM_LIMIT: usize = 500;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistResult {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub thumbnail: String,
    pub items: Vec<SearchResult>,
    /// True if a configured bound or inconsistent server pagination prevented
    /// complete enumeration. Never present a partial first page as complete.
    pub truncated: bool,
    pub unavailable_items: usize,
    pub reported_total: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Audio,
    /// A single progressive file with both tracks. No external muxer is used.
    Video,
}

/// Signed stream URLs are intentionally not included in `Debug` output.
#[derive(Clone)]
pub struct ResolvedMedia {
    pub media: SearchResult,
    pub stream_url: String,
    pub mime_type: String,
    pub extension: String,
    pub content_length: Option<u64>,
    pub bitrate: u64,
    pub has_video: bool,
}

impl std::fmt::Debug for ResolvedMedia {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedMedia")
            .field("media", &self.media)
            .field("mime_type", &self.mime_type)
            .field("extension", &self.extension)
            .field("content_length", &self.content_length)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Default)]
pub struct YoutubeClient {
    client: Arc<OnceCell<Innertube>>,
}

impl YoutubeClient {
    pub fn new() -> Result<Self> {
        Ok(Self::default())
    }

    /// Enumerate a public playlist, up to 500 playable entries, preserving order
    /// and repeated videos. Private/deleted entries are counted separately.
    pub async fn playlist(&self, input: &str) -> Result<PlaylistResult> {
        self.playlist_with_limit(input, PLAYLIST_ITEM_LIMIT, CancellationToken::new())
            .await
    }

    pub async fn playlist_with_limit(
        &self,
        input: &str,
        limit: usize,
        cancel: CancellationToken,
    ) -> Result<PlaylistResult> {
        let url = canonical_playlist_url(input)?;
        let id = Url::parse(&url)
            .expect("canonical URL")
            .query_pairs()
            .find(|(k, _)| k == "list")
            .expect("playlist id")
            .1
            .into_owned();
        if !(1..=PLAYLIST_ITEM_LIMIT).contains(&limit) {
            return Err(MediaError::InvalidInput(
                "El límite de la lista debe estar entre 1 y 500 videos.".into(),
            ));
        }
        if cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        tokio::select! {
            _ = cancel.cancelled() => Err(MediaError::Cancelled),
            response = tokio::time::timeout(Duration::from_secs(120), self.read_playlist(&id, limit)) => {
                response.map_err(|_| MediaError::Network("La consulta de la lista tardó demasiado; no se presentó una lista parcial como completa.".into()))?
            }
        }
    }

    async fn read_playlist(&self, id: &str, limit: usize) -> Result<PlaylistResult> {
        let client = self.client().await?;
        let raw =
            playlist_request(client, serde_json::json!({"browseId":format!("VL{id}")})).await?;
        let page = parse_playlist_browse_response(id, &raw).map_err(youtube_error)?;
        // YouTube now emits continuationItemViewModel in playlist sections.
        // innertube-rs 0.10.1 parses the videos but misses that node's token.
        let first_token = playlist_continuation_token(&raw).or(page.continuation_token);
        let mut result = PlaylistResult {
            id: id.to_owned(),
            title: page.title,
            channel: page.author.unwrap_or_default(),
            thumbnail: page.thumbnail.unwrap_or_default(),
            items: Vec::new(),
            truncated: false,
            unavailable_items: 0,
            reported_total: page.video_count,
        };
        let mut scanned = page.videos.len();
        append_playlist_page(&mut result, page.videos, limit);
        let mut token = first_token.filter(|token| !token.is_empty());
        let mut tokens = HashSet::new();
        let mut pages = 1;
        while let Some(next) = token.take() {
            if result.items.len() >= limit
                || result.truncated
                || pages >= 20
                || !tokens.insert(next.clone())
            {
                result.truncated = true;
                break;
            }
            let raw = playlist_request(client, serde_json::json!({"continuation":next})).await?;
            let page = parse_playlist_continuation_response(&raw).map_err(youtube_error)?;
            scanned += page.videos.len();
            append_playlist_page(&mut result, page.videos, limit);
            token = playlist_continuation_token(&raw)
                .or(page.continuation_token)
                .filter(|token| !token.is_empty());
            pages += 1;
        }
        if result
            .reported_total
            .is_some_and(|total| total as usize > scanned)
        {
            result.truncated = true;
        }
        if result.thumbnail.is_empty() {
            result.thumbnail = result
                .items
                .first()
                .map(|item| item.thumbnail.clone())
                .unwrap_or_default();
        }
        if result.items.is_empty() && result.reported_total != Some(0) {
            return Err(MediaError::Youtube(
                "La lista no contiene videos públicos disponibles o requiere iniciar sesión."
                    .into(),
            ));
        }
        Ok(result)
    }

    async fn client(&self) -> Result<&Innertube> {
        self.client
            .get_or_try_init(|| async {
                tokio::time::timeout(Duration::from_secs(45), Innertube::new())
                    .await
                    .map_err(|_| {
                        MediaError::Network("La conexión con YouTube tardó demasiado.".into())
                    })?
                    .map_err(youtube_error)
            })
            .await
    }

    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let query = query.trim();
        if query.is_empty() || query.len() > 2048 || query.chars().any(char::is_control) {
            return Err(MediaError::InvalidInput(
                "Escribe una búsqueda válida.".into(),
            ));
        }
        if canonical_video_url(query).is_ok() {
            return self.metadata(query).await.map(|video| vec![video]);
        }
        let client = self.client().await?;
        let results = tokio::time::timeout(Duration::from_secs(45), client.search(query, None))
            .await
            .map_err(|_| MediaError::Network("La búsqueda tardó demasiado.".into()))?
            .map_err(youtube_error)?;
        Ok(results
            .items
            .into_iter()
            .filter_map(|result| match result {
                SearchResultItem::Video(video) => Some(SearchResult {
                    url: format!("https://www.youtube.com/watch?v={}", video.video_id),
                    id: video.video_id,
                    title: video.title,
                    thumbnail: video
                        .thumbnails
                        .last()
                        .map(|t| t.url.clone())
                        .unwrap_or_default(),
                    duration_seconds: parse_duration(video.duration.as_deref().unwrap_or("")),
                    channel: video.author,
                    view_count: video.view_count.as_deref().map(parse_views).unwrap_or(0),
                }),
                _ => None,
            })
            .take(limit.clamp(1, 50))
            .collect())
    }

    pub async fn metadata(&self, input: &str) -> Result<SearchResult> {
        let id = video_id(input)?;
        let client = self.client().await?;
        let info = tokio::time::timeout(Duration::from_secs(60), client.get_video_info(&id))
            .await
            .map_err(|_| MediaError::Network("La consulta tardó demasiado.".into()))?
            .map_err(youtube_error)?;
        info.video_details.map(details_to_result).ok_or_else(|| {
            MediaError::Youtube("El video no está disponible para esta sesión.".into())
        })
    }

    pub async fn resolve(&self, input: &str, kind: StreamKind) -> Result<ResolvedMedia> {
        let id = video_id(input)?;
        let client = self.client().await?;
        let options = GetVideoInfoOptions {
            client: Some(innertube_rs::constants::clients::VISIONOS_NAME.into()),
            ..Default::default()
        };
        let info = tokio::time::timeout(
            Duration::from_secs(60),
            client.get_basic_info(&id, Some(&options)),
        )
        .await
        .map_err(|_| MediaError::Network("La resolución del video tardó demasiado.".into()))?
        .map_err(youtube_error)?
        .player_response;
        if info.playability_status.status != "OK" {
            return Err(MediaError::Youtube(
                "El video no está disponible para esta sesión.".into(),
            ));
        }
        if info
            .video_details
            .as_ref()
            .is_some_and(|details| details.is_live == Some(true))
        {
            return Err(MediaError::FormatUnavailable);
        }
        // Prefer AAC/M4A for broad file interoperability. Progressive MP4
        // video must include audio and never requires a subprocess muxer.
        let filter = FormatFilter {
            format_type: match kind {
                StreamKind::Audio => FormatType::AudioOnly,
                StreamKind::Video => FormatType::AudioVideo,
            },
            quality: QualityPreference::Highest,
            container: Some("mp4".into()),
        };
        let format = select_format(&info, &filter).map_err(youtube_error)?;
        if format
            .drm_families
            .as_ref()
            .is_some_and(|drm| !drm.is_empty())
            || format.drm_track_type.is_some()
        {
            return Err(MediaError::FormatUnavailable);
        }
        let stream_url = resolve_stream_url_full(
            format,
            &client.player.decipherer,
            client.session.po_token.as_deref(),
            None,
        )
        .map_err(youtube_error)?;
        let result = ResolvedMedia {
            stream_url,
            mime_type: format.mime_type.clone(),
            extension: if kind == StreamKind::Audio {
                "m4a"
            } else {
                "mp4"
            }
            .into(),
            content_length: format
                .content_length
                .as_deref()
                .and_then(|v| v.parse().ok()),
            bitrate: format.bitrate,
            has_video: kind == StreamKind::Video,
            media: details_to_result(
                info.video_details
                    .clone()
                    .ok_or(MediaError::FormatUnavailable)?,
            ),
        };
        Ok(result)
    }
}

async fn playlist_request(
    client: &Innertube,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    tokio::time::timeout(Duration::from_secs(45), async {
        client
            .session
            .post_innertube("/browse", payload)
            .await
            .map_err(youtube_error)?
            .error_for_status()
            .map_err(crate::network_error)?
            .json()
            .await
            .map_err(crate::network_error)
    })
    .await
    .map_err(|_| {
        MediaError::Network("La página de la lista tardó demasiado; vuelve a intentarlo.".into())
    })?
}

fn playlist_continuation_token(value: &serde_json::Value) -> Option<String> {
    fn command(node: &serde_json::Value) -> Option<String> {
        if let Some(token) = node
            .get("continuationCommand")
            .and_then(|c| c.get("token"))
            .and_then(|t| t.as_str())
            .filter(|t| !t.is_empty())
        {
            return Some(token.into());
        }
        match node {
            serde_json::Value::Object(map) => map.values().find_map(command),
            serde_json::Value::Array(array) => array.iter().find_map(command),
            _ => None,
        }
    }
    if let Some(node) = value
        .get("continuationItemViewModel")
        .or_else(|| value.get("continuationItemRenderer"))
    {
        if let Some(token) = command(node) {
            return Some(token);
        }
    }
    match value {
        serde_json::Value::Object(map) => map.values().find_map(playlist_continuation_token),
        serde_json::Value::Array(array) => array.iter().find_map(playlist_continuation_token),
        _ => None,
    }
}

fn append_playlist_page(result: &mut PlaylistResult, videos: Vec<PlaylistVideoItem>, limit: usize) {
    for video in videos {
        if !video.is_playable || canonical_video_url(&video.id).is_err() {
            result.unavailable_items += 1;
            continue;
        }
        if result.items.len() >= limit {
            result.truncated = true;
            break;
        }
        result.items.push(SearchResult {
            url: format!("https://www.youtube.com/watch?v={}", video.id),
            id: video.id,
            title: video.title,
            channel: video.author,
            thumbnail: video.thumbnail.unwrap_or_default(),
            duration_seconds: video
                .duration_ms
                .map(|ms| ms / 1000)
                .unwrap_or_else(|| parse_duration(video.duration.as_deref().unwrap_or(""))),
            view_count: 0,
        });
    }
}

/// Normalize a playlist ID or a YouTube URL containing `list=`. Video+playlist
/// links retain the playlist intent instead of being reduced to one video.
pub fn canonical_playlist_url(input: &str) -> Result<String> {
    let value = input.trim();
    let invalid =
        || MediaError::InvalidInput("Usa un enlace válido de una lista de YouTube.".into());
    if value.len() > 2048 || value.chars().any(char::is_control) {
        return Err(invalid());
    }
    let valid_id = |id: &str| {
        (2..=128).contains(&id.len())
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            && (id == "WL"
                || id == "LL"
                || (id.len() >= 10
                    && ["PL", "UU", "OLAK5uy", "RD"]
                        .iter()
                        .any(|prefix| id.starts_with(prefix))))
    };
    if valid_id(value) {
        return Ok(format!("https://www.youtube.com/playlist?list={value}"));
    }
    let full = if value.contains("://") {
        value.to_owned()
    } else {
        format!("https://{value}")
    };
    let url = Url::parse(&full).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.port(), None | Some(80) | Some(443))
    {
        return Err(invalid());
    }
    let host = url.host_str().ok_or_else(invalid)?.trim_end_matches('.');
    if !matches!(
        host,
        "youtube.com"
            | "www.youtube.com"
            | "m.youtube.com"
            | "music.youtube.com"
            | "youtu.be"
            | "www.youtu.be"
    ) {
        return Err(invalid());
    }
    let id = url
        .query_pairs()
        .find(|(key, _)| key == "list")
        .map(|(_, id)| id.into_owned())
        .ok_or_else(invalid)?;
    if !valid_id(&id) {
        return Err(invalid());
    }
    Ok(format!("https://www.youtube.com/playlist?list={id}"))
}

fn details_to_result(details: VideoDetails) -> SearchResult {
    SearchResult {
        url: format!("https://www.youtube.com/watch?v={}", details.video_id),
        thumbnail: details
            .thumbnail
            .and_then(|t| t.thumbnails.last().map(|t| t.url.clone()))
            .unwrap_or_else(|| {
                format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", details.video_id)
            }),
        id: details.video_id,
        title: details.title,
        duration_seconds: details.length_seconds.parse().unwrap_or(0),
        channel: details.author,
        view_count: details.view_count.and_then(|v| v.parse().ok()).unwrap_or(0),
    }
}

fn video_id(input: &str) -> Result<String> {
    let canonical = canonical_video_url(input)?;
    Ok(canonical.rsplit('=').next().unwrap_or_default().to_owned())
}

fn parse_duration(value: &str) -> u64 {
    let parts: Vec<_> = value.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return 0;
    }
    parts
        .into_iter()
        .try_fold(0u64, |total, part| {
            total
                .checked_mul(60)?
                .checked_add(part.parse::<u64>().ok()?)
        })
        .unwrap_or(0)
}

fn parse_views(value: &str) -> u64 {
    let raw = value
        .split_whitespace()
        .next()
        .unwrap_or("")
        .replace(',', "");
    let (digits, multiplier) = match raw.chars().last() {
        Some('K') => (&raw[..raw.len() - 1], 1_000.0),
        Some('M') => (&raw[..raw.len() - 1], 1_000_000.0),
        Some('B') => (&raw[..raw.len() - 1], 1_000_000_000.0),
        _ => (raw.as_str(), 1.0),
    };
    digits
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(|v| (v * multiplier) as u64)
        .unwrap_or(0)
}

/// Validate exact YouTube hosts before the extractor performs network requests.
pub fn canonical_video_url(input: &str) -> Result<String> {
    let value = input.trim();
    let invalid = || MediaError::InvalidInput("Usa un enlace de video válido de YouTube.".into());
    if value.len() > 2048 || value.chars().any(char::is_control) {
        return Err(invalid());
    }
    let valid_id = |id: &str| {
        id.len() == 11
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    if valid_id(value) {
        return Ok(format!("https://www.youtube.com/watch?v={value}"));
    }
    let full = if value.contains("://") {
        value.to_owned()
    } else {
        format!("https://{value}")
    };
    let parsed = Url::parse(&full).map_err(|_| invalid())?;
    if !matches!(parsed.scheme(), "https" | "http")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !matches!(parsed.port(), None | Some(80) | Some(443))
    {
        return Err(invalid());
    }
    let host = parsed.host_str().ok_or_else(invalid)?.trim_end_matches('.');
    let id = match host {
        "youtu.be" | "www.youtu.be" => parsed
            .path()
            .trim_matches('/')
            .split('/')
            .next()
            .unwrap_or("")
            .to_owned(),
        "youtube.com"
        | "www.youtube.com"
        | "m.youtube.com"
        | "music.youtube.com"
        | "youtube-nocookie.com"
        | "www.youtube-nocookie.com" => {
            if parsed.path().trim_end_matches('/') == "/watch" {
                parsed
                    .query_pairs()
                    .find(|(key, _)| key == "v")
                    .map(|(_, value)| value.into_owned())
                    .unwrap_or_default()
            } else {
                let parts: Vec<_> = parsed.path().trim_matches('/').split('/').collect();
                if parts.len() == 2 && matches!(parts[0], "shorts" | "embed" | "live" | "v") {
                    parts[1].to_owned()
                } else {
                    String::new()
                }
            }
        }
        _ => return Err(invalid()),
    };
    if !valid_id(&id) {
        return Err(invalid());
    }
    Ok(format!("https://www.youtube.com/watch?v={id}"))
}

fn youtube_error(error: innertube_rs::InnertubeError) -> MediaError {
    // The extractor can include signed URLs or cookies in its error text.
    // Expose a bounded category, never its raw Debug/Display representation.
    let detail = error.to_string().to_ascii_lowercase();
    let message = if detail.contains("429") || detail.contains("too many") {
        "El servicio limitó las solicitudes. Inténtalo más tarde."
    } else if detail.contains("403") || detail.contains("sign in") || detail.contains("bot") {
        "YouTube requiere verificar el acceso. Este motor todavía no admite esa verificación."
    } else if detail.contains("private")
        || detail.contains("unavailable")
        || detail.contains("not found")
    {
        "El video no está disponible para esta sesión."
    } else if detail.contains("cipher")
        || detail.contains("signature")
        || detail.contains("function")
    {
        "El reproductor de YouTube cambió; el extractor requiere una actualización."
    } else {
        "No se pudo resolver el video. Comprueba la conexión o prueba otro enlace."
    };
    MediaError::Youtube(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_playlist_view_model_continuations_are_followed() {
        let modern = serde_json::json!({"items":[{"continuationItemViewModel":{"continuationCommand":{"innertubeCommand":{"continuationCommand":{"token":"second-page","request":"CONTINUATION_REQUEST_TYPE_BROWSE"}}}}}]});
        assert_eq!(
            playlist_continuation_token(&modern).as_deref(),
            Some("second-page")
        );
        let legacy = serde_json::json!({"continuationItems":[{"continuationItemRenderer":{"continuationEndpoint":{"continuationCommand":{"token":"legacy-page"}}}}]});
        assert_eq!(
            playlist_continuation_token(&legacy).as_deref(),
            Some("legacy-page")
        );
        assert!(playlist_continuation_token(
            &serde_json::json!({"continuationCommand":{"token":"unrelated"}})
        )
        .is_none());
    }

    #[test]
    fn playlist_links_preserve_list_intent_and_reject_spoofed_hosts() {
        let id = "PL49CF3715CB9EF31D";
        for input in [
            id.to_owned(),
            format!("https://www.youtube.com/playlist?list={id}"),
            format!("https://youtu.be/dQw4w9WgXcQ?list={id}&index=3"),
            format!("music.youtube.com/watch?v=dQw4w9WgXcQ&list={id}"),
        ] {
            assert_eq!(
                canonical_playlist_url(&input).unwrap(),
                format!("https://www.youtube.com/playlist?list={id}")
            );
        }
        for input in [
            format!("https://youtube.com.evil.test/playlist?list={id}"),
            format!("https://user@youtube.com/playlist?list={id}"),
            format!("https://youtube.com:9999/playlist?list={id}"),
            "https://youtube.com/watch?v=dQw4w9WgXcQ".into(),
            "a normal search".into(),
            "PLhello\nworld".into(),
        ] {
            assert!(canonical_playlist_url(&input).is_err(), "{input}");
        }
    }

    #[test]
    fn playlist_pages_preserve_duplicates_and_mark_limits() {
        let item = |id: &str, playable| PlaylistVideoItem {
            id: id.into(),
            title: id.into(),
            author: "Author".into(),
            author_id: None,
            duration: Some("1:23".into()),
            duration_ms: None,
            thumbnail: None,
            index: None,
            is_playable: playable,
            set_video_id: None,
        };
        let mut result = PlaylistResult {
            id: "PLfixture1234".into(),
            title: "Fixture".into(),
            channel: String::new(),
            thumbnail: String::new(),
            items: Vec::new(),
            truncated: false,
            unavailable_items: 0,
            reported_total: None,
        };
        append_playlist_page(
            &mut result,
            vec![item("dQw4w9WgXcQ", true), item("aqz-KE-bpKQ", false)],
            3,
        );
        assert_eq!(result.unavailable_items, 1);
        append_playlist_page(
            &mut result,
            vec![
                item("dQw4w9WgXcQ", true),
                item("aqz-KE-bpKQ", true),
                item("jNQXAC9IVRw", true),
            ],
            3,
        );
        assert_eq!(result.items.len(), 3);
        assert!(result.truncated);
        assert_eq!(result.items[0].id, result.items[1].id);
        assert_eq!(result.items[2].duration_seconds, 83);
    }

    #[tokio::test]
    async fn playlist_precancellation_does_not_initialize_network_client() {
        let client = YoutubeClient::new().unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            client
                .playlist_with_limit("PL49CF3715CB9EF31D", 500, cancel)
                .await,
            Err(MediaError::Cancelled)
        ));
        assert!(client.client.get().is_none());
    }

    #[test]
    fn parses_search_durations_and_view_counts() {
        assert_eq!(parse_duration("1:10:35"), 4235);
        assert_eq!(parse_duration("10:35"), 635);
        assert_eq!(parse_duration("LIVE"), 0);
        assert_eq!(parse_views("23,520,940 views"), 23_520_940);
        assert_eq!(parse_views("1.2M views"), 1_200_000);
        assert_eq!(parse_views("NaN"), 0);
    }

    #[test]
    fn normalizes_supported_urls() {
        for input in [
            "dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ?t=10",
            "music.youtube.com/watch?v=dQw4w9WgXcQ&list=PL123",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
        ] {
            assert_eq!(
                canonical_video_url(input).unwrap(),
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
            );
        }
    }

    #[test]
    fn rejects_spoofed_hosts_and_credentials() {
        for input in [
            "https://youtube.com.example.org/watch?v=dQw4w9WgXcQ",
            "https://user:password@youtube.com/watch?v=dQw4w9WgXcQ",
            "file:///etc/passwd",
            "https://youtube.com:9999/watch?v=dQw4w9WgXcQ",
            "https://youtu.be/invalid",
            "https://youtube.com/watch?v=dQw4w9WgXcQ\nother",
        ] {
            assert!(canonical_video_url(input).is_err(), "{input}");
        }
    }
}
