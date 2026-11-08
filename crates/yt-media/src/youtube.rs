use crate::{CancellationToken, ConnectionOptions, MediaError, Result};
use innertube_rs::{
    endpoints::player::resolve_stream_url_full,
    endpoints::playlist::{parse_playlist_browse_response, parse_playlist_continuation_response},
    models::playlist::PlaylistVideoItem,
    models::video::{GetVideoInfoOptions, VideoDetails},
    Innertube,
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

/// Maximum requested height; a lower actual resolution is reported explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VideoQuality {
    #[default]
    Best,
    MaxHeight(u32),
}
#[derive(Debug, Clone)]
pub struct ResolvedDownload {
    pub media: SearchResult,
    pub video: ResolvedMedia,
    /// Some means separate DASH tracks must be muxed before publication.
    pub audio: Option<ResolvedMedia>,
    pub height: u32,
    pub quality_label: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvailableFormat {
    pub itag: u32,
    pub mime_type: String,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub bitrate: u64,
    pub content_length: Option<u64>,
    pub audio: bool,
    pub video: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubtitleTrack {
    pub id: String,
    pub language_code: String,
    pub name: String,
    pub automatic: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoDetailsResult {
    pub media: SearchResult,
    pub description: String,
    pub channel_url: String,
    pub is_live: bool,
    pub formats: Vec<AvailableFormat>,
    pub subtitles: Vec<SubtitleTrack>,
}
pub use innertube_rs::models::transcript::Transcript as SubtitleDocument;

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
    pub audio_track_id: Option<String>,
    pub dynamic_range_compressed: bool,
    pub(crate) resolver: Option<YoutubeClient>,
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
    options: Arc<ConnectionOptions>,
    browser_session: Option<crate::BrowserCookieSession>,
}

impl YoutubeClient {
    pub fn new() -> Result<Self> {
        Ok(Self::default())
    }

    pub fn with_options(options: ConnectionOptions) -> Result<Self> {
        options.client(true)?;
        options.cookie_header()?;
        Ok(Self {
            client: Arc::new(OnceCell::new()),
            options: Arc::new(options),
            browser_session: None,
        })
    }
    pub fn with_browser_session(
        options: ConnectionOptions,
        session: crate::BrowserCookieSession,
    ) -> Result<Self> {
        options.client(true)?;
        if options.cookies_file.is_some() {
            return Err(MediaError::InvalidInput(
                "Elige un navegador o un archivo de cookies, no ambos.".into(),
            ));
        }
        Ok(Self {
            client: Arc::new(OnceCell::new()),
            options: Arc::new(options),
            browser_session: Some(session),
        })
    }
    pub(crate) fn http_client(&self) -> Result<reqwest::Client> {
        self.options.client(false)
    }

    /// Download cover art without attaching account cookies. Only YouTube's
    /// image hosts are accepted, including every redirect, and bodies are capped.
    pub async fn fetch_thumbnail(&self, value: &str, cancel: CancellationToken) -> Result<Vec<u8>> {
        use futures_util::StreamExt;
        let url = Url::parse(value).map_err(|_| {
            MediaError::InvalidInput("La carátula no tiene un enlace válido.".into())
        })?;
        if !thumbnail_url(&url) {
            return Err(MediaError::InvalidInput(
                "La carátula debe proceder de YouTube.".into(),
            ));
        }
        let client = self
            .options
            .client_with_policy(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 || !thumbnail_url(attempt.url()) {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))?;
        tokio::select! {
            _=cancel.cancelled()=>Err(MediaError::Cancelled),
            result=async {
                let response=client.get(url).send().await.map_err(crate::network_error)?.error_for_status().map_err(crate::network_error)?;
                const LIMIT:usize=16*1024*1024;
                if response.content_length().is_some_and(|n|n>LIMIT as u64){return Err(MediaError::InvalidInput("La carátula supera 16 MiB.".into()));}
                if !response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v|v.to_str().ok()).is_some_and(|mime|mime.starts_with("image/")) {return Err(MediaError::InvalidInput("La respuesta no es una imagen.".into()));}
                let mut stream=response.bytes_stream();let mut bytes=Vec::new();
                while let Some(chunk)=stream.next().await {
                    let chunk=chunk.map_err(crate::network_error)?;
                    if bytes.len()+chunk.len()>LIMIT{return Err(MediaError::InvalidInput("La carátula supera 16 MiB.".into()));}
                    bytes.extend_from_slice(&chunk);
                }
                if bytes.is_empty(){return Err(MediaError::IncompleteDownload);}
                Ok(bytes)
            }=>result
        }
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
                tokio::time::timeout(Duration::from_secs(90), async {
                    use innertube_rs::core::{
                        player::Player,
                        session::{Session, SessionOptions},
                    };
                    let http_client = self.options.client(true)?;
                    let cookie = match &self.browser_session {
                        Some(session) => Some(session.header()),
                        None => self.options.cookie_header()?,
                    };
                    let mut session = Session::create(SessionOptions {
                        cookie,
                        generate_session_locally: Some(true),
                        retrieve_innertube_config: Some(false),
                        ..Default::default()
                    })
                    .await
                    .map_err(youtube_error)?;
                    session.http_client = http_client;
                    bootstrap_session(&mut session).await?;
                    let player = Player::create(&session.http_client, None)
                        .await
                        .map_err(youtube_error)?;
                    Ok(Innertube {
                        session: Arc::new(session),
                        player: Arc::new(player),
                    })
                })
                .await
                .map_err(|_| {
                    MediaError::Network("La conexión con YouTube tardó demasiado.".into())
                })?
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
        let results = tokio::time::timeout(Duration::from_secs(45), async {
            use futures_util::StreamExt;
            let response = client
                .session
                .post_innertube("/search", serde_json::json!({"query":query}))
                .await
                .map_err(youtube_error)?
                .error_for_status()
                .map_err(crate::network_error)?;
            let mut bytes = Vec::new();
            let mut body = response.bytes_stream();
            while let Some(chunk) = body.next().await {
                let chunk = chunk.map_err(crate::network_error)?;
                if bytes.len() + chunk.len() > 16 * 1024 * 1024 {
                    return Err(MediaError::Youtube(
                        "La respuesta de búsqueda supera el límite admitido.".into(),
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&bytes)
                .map_err(|_| MediaError::Youtube("La búsqueda no entregó datos válidos.".into()))
        })
        .await
        .map_err(|_| MediaError::Network("La búsqueda tardó demasiado.".into()))??;
        Ok(crate::search::parse_results(&results, limit.clamp(1, 50)))
    }

    pub async fn metadata(&self, input: &str) -> Result<SearchResult> {
        self.player_response(input)
            .await?
            .video_details
            .map(details_to_result)
            .ok_or_else(|| {
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
        let streams = info
            .streaming_data
            .as_ref()
            .ok_or(MediaError::FormatUnavailable)?;
        let formats: Vec<_> = streams
            .formats
            .iter()
            .chain(&streams.adaptive_formats)
            .filter(|f| usable_mp4(f))
            .collect();
        let format = if kind == StreamKind::Audio {
            choose_audio_format(&formats, true).ok_or(MediaError::FormatUnavailable)?
        } else {
            let progressive: Vec<_> = formats
                .iter()
                .copied()
                .filter(|f| f.is_audio_video() && playback_format(f))
                .collect();
            choose_video_format(&progressive, VideoQuality::Best)
                .ok_or(MediaError::FormatUnavailable)?
        };
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
            audio_track_id: format.audio_track.as_ref().map(|t| t.id.clone()),
            dynamic_range_compressed: format.is_drc == Some(true),
            resolver: Some(self.clone()),
            media: details_to_result(
                info.video_details
                    .clone()
                    .ok_or(MediaError::FormatUnavailable)?,
            ),
        };
        Ok(result)
    }
    async fn player_response(
        &self,
        input: &str,
    ) -> Result<innertube_rs::models::video::PlayerResponse> {
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
        .map_err(|_| MediaError::Network("La consulta del video tardó demasiado.".into()))?
        .map_err(youtube_error)?
        .player_response;
        if info.playability_status.status != "OK" {
            return Err(MediaError::Youtube(
                "El video no está disponible para esta sesión.".into(),
            ));
        }
        Ok(info)
    }
    fn resolved_format(
        &self,
        client: &Innertube,
        details: &VideoDetails,
        format: &innertube_rs::models::format::StreamingFormat,
    ) -> Result<ResolvedMedia> {
        let stream_url = resolve_stream_url_full(
            format,
            &client.player.decipherer,
            client.session.po_token.as_deref(),
            None,
        )
        .map_err(youtube_error)?;
        Ok(ResolvedMedia {
            media: details_to_result(details.clone()),
            stream_url,
            mime_type: format.mime_type.clone(),
            extension: if format.is_audio_only() { "m4a" } else { "mp4" }.into(),
            content_length: format
                .content_length
                .as_deref()
                .and_then(|v| v.parse().ok()),
            bitrate: format.bitrate,
            has_video: !format.is_audio_only(),
            audio_track_id: format.audio_track.as_ref().map(|t| t.id.clone()),
            dynamic_range_compressed: format.is_drc == Some(true),
            resolver: Some(self.clone()),
        })
    }
    /// Renew the exact original format when continuing a saved download.
    pub async fn resolve_itag(
        &self,
        input: &str,
        itag: u32,
        cancel: CancellationToken,
    ) -> Result<ResolvedMedia> {
        tokio::select! {
            _=cancel.cancelled()=>Err(MediaError::Cancelled),
            result=async {
                let info=self.player_response(input).await?;
                let details=info.video_details.as_ref().ok_or(MediaError::FormatUnavailable)?;
                let streams=info.streaming_data.as_ref().ok_or(MediaError::FormatUnavailable)?;
                let format=streams.formats.iter().chain(&streams.adaptive_formats).find(|f|f.itag==itag && usable_mp4(f)).ok_or(MediaError::FormatUnavailable)?;
                self.resolved_format(self.client().await?,details,format)
            }=>result
        }
    }
    /// Resolve the exact language and dynamic-range variant, since one itag can
    /// identify several dubbed/DRC streams with different bytes.
    pub async fn resolve_resume(
        &self,
        input: &str,
        key: &crate::ResumeKey,
        cancel: CancellationToken,
    ) -> Result<ResolvedMedia> {
        if video_id(input)? != key.media_id {
            return Err(MediaError::InvalidInput(
                "La identidad del vídeo no coincide con el parcial.".into(),
            ));
        }
        tokio::select! {
            _=cancel.cancelled()=>Err(MediaError::Cancelled),
            result=async {
                let info=self.player_response(input).await?;
                let details=info.video_details.as_ref().ok_or(MediaError::FormatUnavailable)?;
                let streams=info.streaming_data.as_ref().ok_or(MediaError::FormatUnavailable)?;
                let format=streams.formats.iter().chain(&streams.adaptive_formats).find(|f|usable_mp4(f) && resume_format_matches(f,key)).ok_or(MediaError::FormatUnavailable)?;
                self.resolved_format(self.client().await?,details,format)
            }=>result
        }
    }
    /// Select the highest available MP4 video within the requested ceiling and
    /// its original AAC audio. No codec conversion or silent quality downgrade.
    pub async fn resolve_download(
        &self,
        input: &str,
        quality: VideoQuality,
        cancel: CancellationToken,
    ) -> Result<ResolvedDownload> {
        self.resolve_download_filtered(input, quality, cancel, false)
            .await
    }

    /// Playback preparation intentionally chooses codecs supported by both
    /// desktop WebViews. Download selection remains independent of this policy.
    pub(crate) async fn resolve_video_preparation(
        &self,
        input: &str,
        quality: VideoQuality,
        cancel: CancellationToken,
    ) -> Result<ResolvedDownload> {
        self.resolve_download_filtered(input, quality, cancel, true)
            .await
    }

    async fn resolve_download_filtered(
        &self,
        input: &str,
        quality: VideoQuality,
        cancel: CancellationToken,
        playback: bool,
    ) -> Result<ResolvedDownload> {
        if let VideoQuality::MaxHeight(height) = quality {
            if !(144..=4320).contains(&height) {
                return Err(MediaError::InvalidInput(
                    "La resolución debe estar entre 144p y 4320p.".into(),
                ));
            }
        }
        tokio::select! {
            _=cancel.cancelled()=>Err(MediaError::Cancelled),
            result=async {
                let info=self.player_response(input).await?;
                let details=info.video_details.as_ref().ok_or(MediaError::FormatUnavailable)?;
                if details.is_live==Some(true){return Err(MediaError::FormatUnavailable);}
                let streaming=info.streaming_data.as_ref().ok_or(MediaError::FormatUnavailable)?;
                let formats:Vec<_>=streaming.formats.iter().chain(&streaming.adaptive_formats).filter(|f|usable_mp4(f) && (!playback || playback_format(f))).collect();
                let video=choose_video_format(&formats,quality).ok_or(MediaError::FormatUnavailable)?;
                let audio=if video.is_audio_video(){None}else{
                    Some(choose_audio_format(&formats,false).ok_or(MediaError::FormatUnavailable)?)
                };
                let client=self.client().await?;
                Ok(ResolvedDownload{media:details_to_result(details.clone()),video:self.resolved_format(client,details,video)?,
                  audio:audio.map(|f|self.resolved_format(client,details,f)).transpose()?,height:video.height.unwrap_or(0),
                  quality_label:video.quality_label.clone().unwrap_or_else(||format!("{}p",video.height.unwrap_or(0)))})
            }=>result
        }
    }
    pub async fn video_details(
        &self,
        input: &str,
        cancel: CancellationToken,
    ) -> Result<VideoDetailsResult> {
        tokio::select! {
            _=cancel.cancelled()=>Err(MediaError::Cancelled),
            result=async {
                let info=self.player_response(input).await?;
                let details=info.video_details.as_ref().ok_or(MediaError::FormatUnavailable)?;
                let formats=info.streaming_data.as_ref().map(|s|s.formats.iter().chain(&s.adaptive_formats).filter(|f|usable_mp4(f)).map(|f|AvailableFormat{
                    itag:f.itag,mime_type:f.mime_type.clone(),height:f.height,fps:f.fps,bitrate:f.bitrate,
                    content_length:f.content_length.as_deref().and_then(|v|v.parse().ok()),audio:f.is_audio_only()||f.is_audio_video(),video:!f.is_audio_only(),
                }).collect()).unwrap_or_default();
                Ok(VideoDetailsResult{media:details_to_result(details.clone()),description:details.short_description.clone().unwrap_or_default(),
                    channel_url:format!("https://www.youtube.com/channel/{}",details.channel_id),is_live:details.is_live==Some(true),formats,
                    subtitles:caption_tracks(&info)?.into_iter().map(|t|SubtitleTrack{id:t.vss_id.unwrap_or_else(||t.language_code.clone()),language_code:t.language_code,name:t.name,automatic:t.kind.as_deref()==Some("asr")}).collect()})
            }=>result
        }
    }
    /// Fetch the exact selected caption track; never substitute another language.
    pub async fn subtitles(
        &self,
        input: &str,
        track_id: &str,
        cancel: CancellationToken,
    ) -> Result<SubtitleDocument> {
        use futures_util::StreamExt;
        tokio::select! {
            _=cancel.cancelled()=>Err(MediaError::Cancelled),
            result=async {
                let info=self.player_response(input).await?;
                let track=caption_tracks(&info)?.into_iter().find(|t|t.vss_id.as_deref().unwrap_or(&t.language_code)==track_id)
                    .ok_or_else(||MediaError::Youtube("La pista de subtítulos seleccionada ya no está disponible.".into()))?;
                let mut url=Url::parse(&track.base_url).map_err(|_|MediaError::FormatUnavailable)?;
                if !crate::connection::is_youtube_url(&url){return Err(MediaError::FormatUnavailable);}
                let pairs:Vec<_>=url.query_pairs().filter(|(k,_)|k!="fmt").map(|(k,v)|(k.into_owned(),v.into_owned())).collect();
                url.query_pairs_mut().clear().extend_pairs(pairs).append_pair("fmt","json3");
                let client=self.client().await?;
                let mut request=client.session.http_client.get(url.clone());
                if url.host_str()==Some("www.youtube.com"){if let Some(cookie)=&client.session.cookie{request=request.header(reqwest::header::COOKIE,cookie);}}
                let response=request.send().await.map_err(crate::network_error)?.error_for_status().map_err(crate::network_error)?;
                let mut bytes=Vec::new();let mut body=response.bytes_stream();
                while let Some(chunk)=body.next().await{let chunk=chunk.map_err(crate::network_error)?;if bytes.len()+chunk.len()>8*1024*1024{return Err(MediaError::InvalidInput("Los subtítulos exceden 8 MiB.".into()));}bytes.extend_from_slice(&chunk);}
                let text=String::from_utf8(bytes).map_err(|_|MediaError::FormatUnavailable)?;
                let result=innertube_rs::endpoints::transcript::parse_transcript_response(&video_id(input)?,&track.language_code,&text).map_err(youtube_error)?;
                if result.segments.is_empty(){return Err(MediaError::Youtube("YouTube no entregó texto para esta pista de subtítulos.".into()));}
                Ok(result)
            }=>result
        }
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

fn choose_audio_format<'a>(
    formats: &[&'a innertube_rs::models::format::StreamingFormat],
    stereo: bool,
) -> Option<&'a innertube_rs::models::format::StreamingFormat> {
    formats
        .iter()
        .copied()
        .filter(|f| f.is_audio_only() && (!stereo || f.audio_channels.unwrap_or(2) <= 2))
        .max_by_key(|f| {
            (
                f.audio_track.as_ref().is_none_or(|t| t.audio_is_default),
                f.is_drc != Some(true),
                f.bitrate,
            )
        })
}
fn resume_format_matches(
    format: &innertube_rs::models::format::StreamingFormat,
    key: &crate::ResumeKey,
) -> bool {
    format.itag == key.itag
        && format
            .content_length
            .as_deref()
            .and_then(|v| v.parse::<u64>().ok())
            == Some(key.total_bytes)
        && format.audio_track.as_ref().map(|t| t.id.as_str()) == key.audio_track_id.as_deref()
        && (format.is_drc == Some(true)) == key.dynamic_range_compressed
}

fn usable_mp4(f: &innertube_rs::models::format::StreamingFormat) -> bool {
    (f.mime_type.starts_with("audio/mp4") || f.mime_type.starts_with("video/mp4"))
        && f.drm_families.as_ref().is_none_or(Vec::is_empty)
        && f.drm_track_type.is_none()
        && f.is_type_otf != Some(true)
}
fn playback_format(f: &innertube_rs::models::format::StreamingFormat) -> bool {
    if f.is_audio_only() {
        f.mime_type.contains("mp4a") && f.audio_channels.unwrap_or(2) <= 2
    } else {
        f.mime_type.contains("avc1") && (!f.is_audio_video() || f.mime_type.contains("mp4a"))
    }
}
fn choose_video_format<'a>(
    formats: &[&'a innertube_rs::models::format::StreamingFormat],
    quality: VideoQuality,
) -> Option<&'a innertube_rs::models::format::StreamingFormat> {
    let maximum = match quality {
        VideoQuality::Best => u32::MAX,
        VideoQuality::MaxHeight(h) => h,
    };
    formats
        .iter()
        .copied()
        .filter(|f| !f.is_audio_only() && f.height.is_some_and(|h| h <= maximum))
        .max_by_key(|f| {
            (
                f.height.unwrap_or(0),
                f.fps.unwrap_or(0),
                f.mime_type.contains("avc1"),
                f.bitrate,
            )
        })
}
fn caption_tracks(
    info: &innertube_rs::models::video::PlayerResponse,
) -> Result<Vec<innertube_rs::models::transcript::TranscriptTrack>> {
    innertube_rs::endpoints::transcript::extract_caption_tracks_from_player(
        &serde_json::json!({"captions":info.captions}),
    )
    .map_err(youtube_error)
}

/// Upstream Session::create cannot accept a configured HTTP client. Initialize
/// locally, then retrieve the same public bootstrap using our bounded client.
async fn bootstrap_session(session: &mut innertube_rs::core::session::Session) -> Result<()> {
    let response = session
        .http_client
        .get("https://www.youtube.com/sw.js_data")
        .header(
            reqwest::header::USER_AGENT,
            innertube_rs::constants::DEFAULT_USER_AGENT,
        )
        .header(reqwest::header::REFERER, "https://www.youtube.com/sw.js")
        .send()
        .await
        .map_err(crate::network_error)?
        .error_for_status()
        .map_err(crate::network_error)?;
    let text = response.text().await.map_err(crate::network_error)?;
    let data: serde_json::Value = serde_json::from_str(text.trim_start_matches(")]}'").trim())
        .map_err(|_| MediaError::Youtube("YouTube entregó datos de sesión no válidos.".into()))?;
    let config = data
        .pointer("/0/2")
        .ok_or_else(|| MediaError::Youtube("YouTube no entregó datos de sesión.".into()))?;
    let device = config
        .pointer("/0/0")
        .ok_or_else(|| MediaError::Youtube("YouTube no entregó datos de dispositivo.".into()))?;
    let value = |n: usize| {
        device
            .get(n)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    if let Some(key) = config.get(1).and_then(serde_json::Value::as_str) {
        session.api_key = key.into();
    }
    let client = &mut session.context.client;
    if let Some(v) = value(0) {
        client.hl = v;
    }
    if let Some(v) = value(1) {
        client.gl = v;
    }
    client.remote_host = value(3);
    client.visitor_data = value(13);
    if let Some(v) = value(16) {
        client.client_version = v;
    }
    if let Some(v) = value(17) {
        client.os_name = v;
    }
    if let Some(v) = value(18) {
        client.os_version = v;
    }
    client.device_make = value(11);
    client.device_model = value(12);
    client.time_zone = value(79);
    client.browser_name = value(86);
    client.browser_version = value(87);
    client.device_experiment_id = value(103);
    client.rollout_token = value(107);
    if let Some(v) = device
        .get(61)
        .and_then(serde_json::Value::as_array)
        .and_then(|v| v.last())
        .and_then(serde_json::Value::as_str)
    {
        client.config_info = Some(innertube_rs::models::context::ConfigInfo {
            app_install_data: Some(v.into()),
            ..Default::default()
        });
    }
    if let Some(cookie) = &session.cookie {
        let response = session
            .http_client
            .get("https://www.youtube.com/")
            .header(reqwest::header::COOKIE, cookie)
            .header(
                reqwest::header::USER_AGENT,
                innertube_rs::constants::DEFAULT_USER_AGENT,
            )
            .send()
            .await
            .map_err(crate::network_error)?
            .error_for_status()
            .map_err(crate::network_error)?;
        let html = response.text().await.map_err(crate::network_error)?;
        // Read only the one JSON string required by SID authorization.
        if let Some(tail) = html
            .split("\"DATASYNC_ID\"")
            .nth(1)
            .and_then(|s| s.trim_start().strip_prefix(':'))
        {
            let mut strings =
                serde_json::Deserializer::from_str(tail.trim_start()).into_iter::<String>();
            if let Some(Ok(id)) = strings.next() {
                let (first, second) = id.split_once("||").unwrap_or((&id, ""));
                session.user_session_id =
                    Some(if second.is_empty() { first } else { second }.to_owned())
                        .filter(|v| !v.is_empty());
            }
        }
    }
    Ok(())
}

fn thumbnail_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url.host_str().is_some_and(|h| {
            h == "ytimg.com"
                || h.ends_with(".ytimg.com")
                || h == "yt3.ggpht.com"
                || h == "yt3.googleusercontent.com"
        })
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

pub(crate) fn parse_duration(value: &str) -> u64 {
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

pub(crate) fn parse_views(value: &str) -> u64 {
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
    fn playback_chooses_avc_aac_without_changing_original_download_quality() {
        use innertube_rs::models::format::StreamingFormat;
        let av1 = StreamingFormat {
            height: Some(2160),
            mime_type: "video/mp4; codecs=\"av01.0.12M.08\"".into(),
            ..Default::default()
        };
        let avc = StreamingFormat {
            height: Some(720),
            mime_type: "video/mp4; codecs=\"avc1.64001f\"".into(),
            ..Default::default()
        };
        let audio = StreamingFormat {
            mime_type: "audio/mp4; codecs=\"mp4a.40.2\"".into(),
            audio_channels: Some(2),
            ..Default::default()
        };
        let surround = StreamingFormat {
            audio_channels: Some(6),
            ..audio.clone()
        };
        let formats = vec![&av1, &avc, &audio, &surround];
        assert_eq!(
            choose_video_format(&formats, VideoQuality::Best)
                .unwrap()
                .height,
            Some(2160)
        );
        let playable: Vec<_> = formats
            .iter()
            .copied()
            .filter(|f| playback_format(f))
            .collect();
        assert_eq!(
            choose_video_format(&playable, VideoQuality::Best)
                .unwrap()
                .height,
            Some(720)
        );
        assert!(playback_format(&audio));
        assert!(!playback_format(&surround));
    }

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
    #[test]
    fn quality_selection_respects_ceiling_and_prefers_compatible_codec_at_equal_resolution() {
        use innertube_rs::models::format::StreamingFormat;
        let make = |height, fps, mime: &str, bitrate| StreamingFormat {
            height: Some(height),
            fps: Some(fps),
            mime_type: mime.into(),
            bitrate,
            ..Default::default()
        };
        let formats = [
            make(2160, 60, "video/mp4; codecs=av01", 100),
            make(1080, 60, "video/mp4; codecs=av01", 500),
            make(1080, 60, "video/mp4; codecs=avc1", 200),
            make(360, 30, "video/mp4; codecs=avc1", 50),
        ];
        let refs: Vec<_> = formats.iter().collect();
        assert_eq!(
            choose_video_format(&refs, VideoQuality::Best)
                .unwrap()
                .height,
            Some(2160)
        );
        assert_eq!(
            choose_video_format(&refs, VideoQuality::MaxHeight(1080))
                .unwrap()
                .bitrate,
            200
        );
        assert_eq!(
            choose_video_format(&refs, VideoQuality::MaxHeight(720))
                .unwrap()
                .height,
            Some(360)
        );
        assert!(choose_video_format(&refs, VideoQuality::MaxHeight(144)).is_none());
        let mut encrypted = formats[0].clone();
        encrypted.drm_families = Some(vec!["WIDEVINE".into()]);
        assert!(!usable_mp4(&encrypted));
    }
    #[test]
    fn thumbnail_allowlist_rejects_credentials_private_hosts_and_lookalikes() {
        for bad in [
            "http://i.ytimg.com/cover.jpg",
            "https://i.ytimg.com.evil.test/x",
            "https://user:secret@i.ytimg.com/x",
            "https://127.0.0.1/x",
        ] {
            assert!(!thumbnail_url(&Url::parse(bad).unwrap()));
        }
        assert!(thumbnail_url(
            &Url::parse("https://i.ytimg.com/vi/example/hqdefault.jpg").unwrap()
        ));
    }
    #[test]
    fn repeated_itags_keep_language_and_drc_identity_during_resume() {
        use innertube_rs::models::format::{AudioTrack, StreamingFormat};
        let format =
            |language: &str, default: bool, drc: bool, length: u64, bitrate: u64| StreamingFormat {
                itag: 140,
                mime_type: "audio/mp4; codecs=mp4a.40.2".into(),
                content_length: Some(length.to_string()),
                bitrate,
                audio_channels: Some(2),
                is_drc: Some(drc),
                audio_track: Some(AudioTrack {
                    id: language.into(),
                    display_name: language.into(),
                    audio_is_default: default,
                }),
                ..Default::default()
            };
        let variants = [
            format("es.4", false, false, 100u64, 500000u64),
            format("en.4", true, true, 200, 130000),
            format("en.4", true, false, 200, 120000),
        ];
        let refs: Vec<_> = variants.iter().collect();
        assert_eq!(choose_audio_format(&refs, true).unwrap().bitrate, 120000);
        let key = crate::ResumeKey {
            media_id: "dQw4w9WgXcQ".into(),
            itag: 140,
            total_bytes: 200,
            audio_track_id: Some("en.4".into()),
            dynamic_range_compressed: false,
        };
        assert!(!resume_format_matches(&variants[0], &key));
        assert!(!resume_format_matches(&variants[1], &key));
        assert!(resume_format_matches(&variants[2], &key));
    }
}
