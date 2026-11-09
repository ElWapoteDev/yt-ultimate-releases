//! Application-owned media operations. No subprocesses or local HTTP service.

mod browser_cookies;
mod connection;
mod decoder;
mod download;
mod local_video;
mod mux;
mod playback;
mod prepared_video;
mod resume;
mod search;
#[cfg(feature = "native-stems")]
mod stems;
mod streaming;
mod tempo;
mod youtube;

pub use browser_cookies::{read_browser_cookies, BrowserCookieSession};
pub use connection::ConnectionOptions;
pub use decoder::{decode_file, DecodedAudio};
pub use download::{safe_file_name, DownloadProgress, DownloadReceipt, Downloader};
pub use local_video::{prepare_local_video, PreparedLocalVideo};
pub use mux::{mux_mp4, MuxProgress};
pub use playback::{AudioPlayer, PlaybackSession, PlaybackSnapshot};
pub use prepared_video::PreparedVideo;
pub use resume::{ResumeKey, ResumeStatus};
#[cfg(feature = "native-stems")]
pub use stems::{StemProgress, StemReceipt, StemSeparator};
pub use streaming::{open_http_audio, StreamingHandle, StreamingSnapshot};
pub use tokio_util::sync::CancellationToken;
pub use youtube::{
    canonical_playlist_url, canonical_video_url, AvailableFormat, PlaylistResult, ResolvedDownload,
    ResolvedMedia, SearchResult, StreamKind, SubtitleDocument, SubtitleTrack, VideoDetailsResult,
    VideoQuality, YoutubeClient, PLAYLIST_ITEM_LIMIT,
};

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("{0}")]
    InvalidInput(String),
    #[error("La conexión falló: {0}")]
    Network(String),
    #[error("YouTube no pudo entregar este contenido: {0}")]
    Youtube(String),
    #[error("No hay un formato compatible disponible para este contenido.")]
    FormatUnavailable,
    #[error("La operación fue cancelada.")]
    Cancelled,
    #[error("El archivo de destino ya existe; no se ha reemplazado.")]
    DestinationExists,
    #[error("La descarga está incompleta o no contiene un archivo multimedia.")]
    IncompleteDownload,
    #[error("No se pudo acceder al archivo: {0}")]
    Io(#[from] std::io::Error),
    #[error("No se pudo reproducir el audio: {0}")]
    Playback(String),
    #[error("No se pudo procesar el audio: {0}")]
    Processing(String),
}

pub type Result<T> = std::result::Result<T, MediaError>;

fn network_error(error: reqwest::Error) -> MediaError {
    MediaError::Network(error.without_url().to_string())
}
