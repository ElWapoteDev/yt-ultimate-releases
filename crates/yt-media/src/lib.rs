//! Application-owned media operations. No subprocesses or local HTTP service.

mod album_art;
mod analysis;
mod browser_cookies;
mod catalog;
mod connection;
mod credits;
mod crossfade;
mod deck_stems;
mod decoder;
mod download;
mod local_video;
mod lrclib;
mod mix;
mod mux;
mod playback;
mod prepared_video;
mod resume;
mod search;
#[cfg(any(
    feature = "native-stems",
    all(feature = "metal-stems", target_os = "macos")
))]
mod stems;
mod streaming;
mod suggest;
mod tempo;
mod up_next;
mod youtube;

pub use album_art::{sized_artwork, thumbnail_video_id};
pub use analysis::{analyze_file, Analyzer};
pub use browser_cookies::{read_browser_cookies, BrowserCookieSession};
pub use catalog::{
    AlbumDetails, AlbumSummary, ArtistProfile, ArtistSummary, CatalogSearch, LiveEvent,
    ReleaseKind, SongContext, CATALOG_KIND_LIMIT,
};
pub use connection::ConnectionOptions;
pub use crossfade::{Crossfade, FadeCurve, Mix, Transition, MAX_CROSSFADE};
pub use deck_stems::{Stem, MAX_STEM_SECONDS};
pub use decoder::{decode_file, DecodedAudio};
pub use download::{safe_file_name, DownloadProgress, DownloadReceipt, Downloader};
pub use local_video::{prepare_local_video, PreparedLocalVideo};
pub use lrclib::{LrclibClient, LRCLIB_SEARCH_LIMIT};
pub use mux::{mux_mp4, MuxProgress};
pub use playback::{AudioPlayer, Blend, PlaybackSession, PlaybackSnapshot, PreparedStream};
pub use prepared_video::PreparedVideo;
pub use resume::{ResumeKey, ResumeStatus};
#[cfg(any(
    feature = "native-stems",
    all(feature = "metal-stems", target_os = "macos")
))]
pub use stems::{StemProgress, StemReceipt, StemSeparator, STEM_MODEL_FILENAME, STEM_MODEL_LABEL};
pub use streaming::{open_http_audio, StreamingHandle, StreamingSnapshot};
pub use tempo::{stretch, transpose, MAX_TRANSPOSE_SEMITONES};
pub use tokio_util::sync::CancellationToken;
pub use up_next::{UpNext, UP_NEXT_ITEM_LIMIT};
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
    /// The service asked clients to slow down; retry after `retry_after`.
    #[error("El servicio está ocupado; vuelve a intentarlo más tarde.")]
    Busy { retry_after: std::time::Duration },
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
