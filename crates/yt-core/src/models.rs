use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub filename: String,
    pub path: PathBuf,
    pub media_type: String,
    pub extension: String,
    pub duration: f64,
    pub size_bytes: u64,
    pub artwork_url: String,
    pub favorite: bool,
    pub missing: bool,
    pub created_at: f64,
    pub source_url: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImportMode {
    #[default]
    Copy,
    Reference,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportReport {
    pub tracks: Vec<Track>,
    pub errors: Vec<ImportFailure>,
    pub playlists: Vec<Playlist>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportFailure {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub description: String,
    pub track_count: u64,
}

/// Explicit library metadata layered over the complete source-file tags.
/// `None` keeps the source value; `Some("")` deliberately clears that field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MediaMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub source_url: Option<String>,
    pub artwork_path: Option<PathBuf>,
    pub preserve_source_tags: bool,
}

impl Default for MediaMetadata {
    fn default() -> Self {
        Self {
            title: None,
            artist: None,
            album: None,
            source_url: None,
            artwork_path: None,
            preserve_source_tags: true,
        }
    }
}

impl From<&Track> for MediaMetadata {
    fn from(track: &Track) -> Self {
        Self {
            title: Some(track.title.clone()),
            artist: Some(track.artist.clone()),
            album: Some(track.album.clone()),
            source_url: (!track.source_url.is_empty()).then(|| track.source_url.clone()),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistExportReport {
    pub path: PathBuf,
    pub exported_tracks: usize,
    pub skipped: Vec<ImportFailure>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanupReport {
    pub files_removed: u64,
    pub bytes_removed: u64,
    pub errors: Vec<ImportFailure>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Error,
    Canceled,
    Interrupted,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Error => "error",
            Self::Canceled => "canceled",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending | Self::Running)
    }

    pub(crate) fn parse(value: &str) -> Self {
        match value {
            "pending" | "queued" => Self::Pending,
            "running" | "cancelling" | "processing" => Self::Running,
            "completed" | "succeeded" => Self::Completed,
            "canceled" | "cancelled" => Self::Canceled,
            "interrupted" => Self::Interrupted,
            _ => Self::Error,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub status: JobStatus,
    pub progress: f64,
    pub message: String,
    pub payload: serde_json::Value,
    pub result: serde_json::Value,
    pub error: String,
    pub created_at: f64,
    pub updated_at: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlaybackState {
    pub queue: Vec<String>,
    /// Unresolved remote queue entries. Store canonical source URLs only;
    /// signed transport URLs and credentials do not belong in playback state.
    #[serde(default)]
    pub remote_tracks: Vec<Track>,
    pub current_index: Option<usize>,
    pub position_seconds: f64,
    pub volume: f64,
    pub playback_rate: f64,
    pub shuffle: bool,
    pub repeat: String,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            queue: Vec::new(),
            remote_tracks: Vec::new(),
            current_index: None,
            position_seconds: 0.0,
            volume: 1.0,
            playback_rate: 1.0,
            shuffle: false,
            repeat: "off".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryEntry {
    pub track: Track,
    pub played_at: f64,
    pub position_seconds: f64,
}
