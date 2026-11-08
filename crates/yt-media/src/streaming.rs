//! Seekable HTTP audio backed by a bounded-range disk cache. The decoder can
//! start after its first packet; the background reader prefetches ahead of it.
use crate::{
    download::parse_content_range, network_error, CancellationToken, DecodedAudio, MediaError,
    ResolvedMedia, Result,
};
use futures_util::StreamExt;
use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Seek, SeekFrom},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    time::Duration,
};
use symphonia::core::io::MediaSource;
use tokio::{
    io::{AsyncSeekExt, AsyncWriteExt},
    sync::mpsc,
};
use url::Url;

const CHUNK: u64 = 256 * 1024;
const EXPIRED_URL: &str = "El enlace del archivo venció; vuelve a intentarlo para renovarlo.";

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StreamingSnapshot {
    pub buffered_bytes: u64,
    pub total_bytes: u64,
    pub complete: bool,
    pub buffering: bool,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct StreamingHandle {
    cancel: CancellationToken,
    state: Arc<Mutex<StreamingSnapshot>>,
}
impl StreamingHandle {
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
    pub fn snapshot(&self) -> StreamingSnapshot {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

struct CacheState {
    chunks: HashSet<u64>,
    error: Option<String>,
}
struct Cache {
    file: tempfile::NamedTempFile,
    state: Mutex<CacheState>,
    ready: Condvar,
    stats: Arc<Mutex<StreamingSnapshot>>,
    cancel: CancellationToken,
    total: u64,
}
struct HttpSource {
    cache: Arc<Cache>,
    file: File,
    position: u64,
    requests: mpsc::UnboundedSender<u64>,
    seekable: Arc<AtomicBool>,
}

/// Open and decode an HTTP audio source without waiting for the complete file.
/// Only signed URLs returned by the resolver are refreshed; no cookies are read.
pub async fn open_http_audio(
    media: ResolvedMedia,
    cancel: CancellationToken,
) -> Result<(DecodedAudio, StreamingHandle)> {
    if cancel.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    let url = validate_url(&media.stream_url)?;
    let client = match &media.resolver {
        Some(resolver) => resolver.http_client()?,
        None => crate::ConnectionOptions::default().client(false)?,
    };
    let total = match media.content_length.or_else(|| {
        url.query_pairs()
            .find(|(k, _)| k == "clen")
            .and_then(|(_, v)| v.parse().ok())
    }) {
        Some(total) if total > 0 => total,
        _ => {
            tokio::select! {_ = cancel.cancelled()=>return Err(MediaError::Cancelled), total=discover_size(&client,&url)=>total?}
        }
    };
    let local_cancel = cancel.child_token();
    let stats = Arc::new(Mutex::new(StreamingSnapshot {
        total_bytes: total,
        buffering: true,
        ..Default::default()
    }));
    let handle = StreamingHandle {
        cancel: local_cancel.clone(),
        state: stats.clone(),
    };
    let file = tempfile::Builder::new()
        .prefix("ytu-stream-")
        .suffix(".cache")
        .tempfile()?;
    let reader = File::open(file.path())?;
    let cache = Arc::new(Cache {
        file,
        state: Mutex::new(CacheState {
            chunks: HashSet::new(),
            error: None,
        }),
        ready: Condvar::new(),
        stats,
        cancel: local_cancel,
        total,
    });
    let (requests, receiver) = mpsc::unbounded_channel();
    let worker_cache = cache.clone();
    let extension = media.extension.clone();
    tokio::spawn(async move {
        run_cache(worker_cache, client, url, media, receiver).await;
    });
    // A progressive MP4 already carries its sample table in the leading moov.
    // Symphonia otherwise scans every top-level atom, forcing all fragmented
    // media ranges to download before its first packet. Probe incrementally,
    // then expose range seeking through the same source for playback.
    let seekable = Arc::new(AtomicBool::new(false));
    let source = HttpSource {
        cache,
        file: reader,
        position: 0,
        requests,
        seekable: seekable.clone(),
    };
    let decoded =
        tokio::task::spawn_blocking(move || DecodedAudio::open(Box::new(source), Some(&extension)))
            .await
            .map_err(|e| MediaError::Playback(e.to_string()))??;
    seekable.store(true, Ordering::Release);
    Ok((decoded, handle))
}

fn validate_url(value: &str) -> Result<Url> {
    let url = Url::parse(value)
        .map_err(|_| MediaError::InvalidInput("El enlace de audio no es válido.".into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(MediaError::InvalidInput(
            "El audio necesita un enlace HTTP sin credenciales.".into(),
        ));
    }
    Ok(url)
}

async fn discover_size(client: &reqwest::Client, url: &Url) -> Result<u64> {
    let response = client
        .get(url.clone())
        .header(reqwest::header::RANGE, "bytes=0-0")
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .map_err(network_error)?
        .error_for_status()
        .map_err(network_error)?;
    if let Some(value) = response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
    {
        return parse_content_range(value).map(|(_, _, total)| total);
    }
    response
        .content_length()
        .filter(|n| *n > 0)
        .ok_or_else(|| MediaError::Network("El servidor no indicó el tamaño del audio.".into()))
}

async fn run_cache(
    cache: Arc<Cache>,
    client: reqwest::Client,
    mut url: Url,
    media: ResolvedMedia,
    mut requests: mpsc::UnboundedReceiver<u64>,
) {
    let result:Result<()>=async {
        let mut file=tokio::fs::File::from_std(cache.file.as_file().try_clone()?);
        let chunks=cache.total.div_ceil(CHUNK);let mut next=0;let mut refreshed=false;
        loop {
            if cache.cancel.is_cancelled(){return Err(MediaError::Cancelled);}
            while let Ok(requested)=requests.try_recv(){next=requested.min(chunks-1);}
            {
                let state=cache.state.lock().unwrap_or_else(|e|e.into_inner());
                if state.chunks.len() as u64==chunks {return Ok(());}
                if state.chunks.contains(&next){next=(0..chunks).find(|index|!state.chunks.contains(index)).expect("incomplete cache");}
            }
            let start=next*CHUNK;let end=(start+CHUNK-1).min(cache.total-1);
            let bytes=tokio::select!{
                biased;
                _=cache.cancel.cancelled()=>return Err(MediaError::Cancelled),
                requested=requests.recv()=>{if let Some(requested)=requested{next=requested.min(chunks-1);continue;}else{return Err(MediaError::Cancelled);}},
                response=fetch_range_retry(&client,&url,start,end,cache.total)=>match response{
                    Ok(bytes)=>bytes,
                    Err(MediaError::Network(message)) if message==EXPIRED_URL && !refreshed && media.resolver.is_some()=>{
                        let resolver=media.resolver.as_ref().expect("checked resolver");
                        let key=crate::ResumeKey::for_media(&media)?;
                        let refreshed_media=resolver.resolve_resume(&media.media.url,&key,cache.cancel.clone()).await?;
                        let refreshed_url=validate_url(&refreshed_media.stream_url)?;
                        let itag=|url:&Url|url.query_pairs().find(|(k,_)|k=="itag").map(|(_,v)|v.into_owned());
                        if refreshed_media.content_length!=Some(cache.total) || itag(&url)!=itag(&refreshed_url){return Err(MediaError::Network("El audio cambió al renovar el enlace; vuelve a reproducirlo.".into()));}
                        url=refreshed_url;refreshed=true;continue;
                    },
                    Err(error)=>return Err(error),
                }
            };
            file.seek(SeekFrom::Start(start)).await?;file.write_all(&bytes).await?;file.flush().await?;
            cache.state.lock().unwrap_or_else(|e|e.into_inner()).chunks.insert(next);
            {let mut stats=cache.stats.lock().unwrap_or_else(|e|e.into_inner());stats.buffered_bytes+=bytes.len() as u64;stats.complete=stats.buffered_bytes==cache.total;}
            cache.ready.notify_all();next=(next+1)%chunks;
        }
    }.await;
    if let Err(error) = result {
        if cache.cancel.is_cancelled() {
            cache
                .stats
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .buffering = false;
            cache.ready.notify_all();
            return;
        }
        let message = error.to_string();
        cache.state.lock().unwrap_or_else(|e| e.into_inner()).error = Some(message.clone());
        cache.stats.lock().unwrap_or_else(|e| e.into_inner()).error = Some(message);
    }
    cache.ready.notify_all();
}

pub(crate) async fn fetch_range_retry(
    client: &reqwest::Client,
    url: &Url,
    start: u64,
    end: u64,
    total: u64,
) -> Result<Vec<u8>> {
    for attempt in 0..3 {
        match fetch_range(client, url, start, end, total).await {
            Ok(bytes) => return Ok(bytes),
            Err(MediaError::Network(message)) if message == EXPIRED_URL => {
                return Err(MediaError::Network(message))
            }
            Err(error) if attempt == 2 => return Err(error),
            Err(_) => tokio::time::sleep(Duration::from_millis(150 * (attempt + 1))).await,
        }
    }
    unreachable!()
}

pub(crate) async fn fetch_range(
    client: &reqwest::Client,
    url: &Url,
    start: u64,
    end: u64,
    total: u64,
) -> Result<Vec<u8>> {
    let mut request_url = url.clone();
    let query_range = url
        .host_str()
        .is_some_and(|h| h.ends_with(".googlevideo.com"));
    if query_range {
        let pairs: Vec<(String, String)> = request_url
            .query_pairs()
            .filter(|(k, _)| k != "range" && k != "rn")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        request_url.set_query(None);
        request_url
            .query_pairs_mut()
            .extend_pairs(pairs)
            .append_pair("range", &format!("{start}-{end}"))
            .append_pair("rn", &(start / CHUNK).to_string());
    }
    let mut request = client
        .get(request_url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity");
    if !query_range {
        request = request.header(reqwest::header::RANGE, format!("bytes={start}-{end}"));
    }
    let response = request.send().await.map_err(network_error)?;
    if matches!(response.status().as_u16(), 403 | 410) {
        return Err(MediaError::Network(EXPIRED_URL.into()));
    }
    let response = response.error_for_status().map_err(network_error)?;
    let expected = end - start + 1;
    if !query_range {
        if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
            let value = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .ok_or(MediaError::IncompleteDownload)?;
            if parse_content_range(value)? != (start, end, total) {
                return Err(MediaError::IncompleteDownload);
            }
        } else if start != 0 || expected != total {
            return Err(MediaError::Network(
                "El servidor no admite reproducción por rangos.".into(),
            ));
        }
    }
    if response.content_length().is_some_and(|n| n != expected) {
        return Err(MediaError::IncompleteDownload);
    }
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if mime.starts_with("text/") || mime.contains("json") {
        return Err(MediaError::IncompleteDownload);
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::with_capacity(expected as usize);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(network_error)?;
        if bytes.len() + chunk.len() > expected as usize {
            return Err(MediaError::IncompleteDownload);
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() as u64 != expected {
        return Err(MediaError::IncompleteDownload);
    }
    Ok(bytes)
}

impl Read for HttpSource {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() || self.position >= self.cache.total {
            return Ok(0);
        }
        let chunk = self.position / CHUNK;
        let mut state = self.cache.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.chunks.contains(&chunk) {
            self.cache
                .stats
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .buffering = true;
            self.requests
                .send(chunk)
                .map_err(|_| std::io::Error::other("La conexión de audio se cerró."))?;
        }
        while !state.chunks.contains(&chunk) {
            if self.cache.cancel.is_cancelled() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "La reproducción fue cancelada.",
                ));
            }
            if let Some(error) = &state.error {
                return Err(std::io::Error::other(error.clone()));
            }
            state = self
                .cache
                .ready
                .wait_timeout(state, Duration::from_millis(200))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        drop(state);
        self.cache
            .stats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .buffering = false;
        let count = buffer
            .len()
            .min((CHUNK - self.position % CHUNK) as usize)
            .min((self.cache.total - self.position) as usize);
        self.file.seek(SeekFrom::Start(self.position))?;
        self.file.read_exact(&mut buffer[..count])?;
        self.position += count as u64;
        Ok(count)
    }
}
impl Seek for HttpSource {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let value = match position {
            SeekFrom::Start(value) => value as i128,
            SeekFrom::End(value) => self.cache.total as i128 + value as i128,
            SeekFrom::Current(value) => self.position as i128 + value as i128,
        };
        if value < 0 || value > self.cache.total as i128 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Posición de audio fuera de rango.",
            ));
        }
        self.position = value as u64;
        Ok(self.position)
    }
}
impl MediaSource for HttpSource {
    fn is_seekable(&self) -> bool {
        self.seekable.load(Ordering::Acquire)
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.cache.total)
    }
}
impl Drop for HttpSource {
    fn drop(&mut self) {
        self.cache.cancel.cancel();
        self.cache.ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::Source;
    use std::{
        io::Write,
        net::TcpListener,
        sync::atomic::{AtomicBool, Ordering},
    };

    #[tokio::test]
    async fn playback_and_seek_work_before_all_ranges_arrive() {
        let frames = 8_000u32 * 180;
        let mut wav = Vec::with_capacity(frames as usize * 2 + 44);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(frames * 2 + 36).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(frames * 2).to_le_bytes());
        for i in 0..frames {
            let value =
                ((i as f64 * std::f64::consts::TAU * 440.0 / 8000.0).sin() * 12000.0) as i16;
            wav.extend_from_slice(&value.to_le_bytes());
        }
        let len = wav.len() as u64;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let server = std::thread::spawn(move || {
            while !stop_thread.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut request = [0; 4096];
                let count = stream.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..count]).to_ascii_lowercase();
                let Some(range) = request
                    .lines()
                    .find_map(|line| line.strip_prefix("range: bytes="))
                else {
                    continue;
                };
                let Some((start, end)) = range
                    .split_once('-')
                    .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)))
                else {
                    continue;
                };
                std::thread::sleep(Duration::from_millis(80));
                let body = &wav[start..=end];
                let header=format!("HTTP/1.1 206 Partial Content\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",body.len(),start,end,wav.len());
                let _ = stream
                    .write_all(header.as_bytes())
                    .and_then(|_| stream.write_all(body));
            }
        });
        let media = ResolvedMedia {
            media: crate::SearchResult {
                id: "fixture".into(),
                url: String::new(),
                title: "fixture".into(),
                thumbnail: String::new(),
                duration_seconds: 180,
                channel: String::new(),
                view_count: 0,
            },
            stream_url: format!("http://{address}/audio.wav"),
            mime_type: "audio/wav".into(),
            extension: "wav".into(),
            content_length: Some(len),
            bitrate: 128000,
            has_video: false,
            audio_track_id: None,
            dynamic_range_compressed: false,
            resolver: None,
        };
        let (mut decoder, handle) = open_http_audio(media, CancellationToken::new())
            .await
            .unwrap();
        assert!(
            !handle.snapshot().complete,
            "decoder must start before the entire download"
        );
        let initial = handle.snapshot().buffered_bytes;
        let decoder = tokio::task::spawn_blocking(move || {
            assert_eq!(decoder.by_ref().take(8000).count(), 8000);
            decoder.try_seek(Duration::from_secs(150)).unwrap();
            assert_eq!(decoder.by_ref().take(8000).count(), 8000);
            assert!(decoder.decoding_error().is_none());
            decoder
        })
        .await
        .unwrap();
        assert!(initial < len / 2);
        assert!(
            !handle.snapshot().complete,
            "seek should request its target before sequential prefetch finishes"
        );
        handle.cancel();
        drop(decoder);
        stop.store(true, Ordering::Release);
        server.join().unwrap();
    }
}
