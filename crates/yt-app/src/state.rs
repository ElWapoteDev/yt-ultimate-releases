use dioxus::prelude::*;
use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use yt_core::{AppCore, ImportMode, Job, JobStatus, PlaybackState, Playlist, Settings, Track};
use yt_media::{
    AudioPlayer, CancellationToken, Downloader, SearchResult, StreamKind, YoutubeClient,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Home,
    Search,
    Library,
    Playlists,
    Stems,
    Downloads,
    Settings,
}

impl Page {
    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "Inicio",
            Self::Search => "Buscar",
            Self::Library => "Canciones",
            Self::Playlists => "Playlists",
            Self::Stems => "Stems",
            Self::Downloads => "Descargas",
            Self::Settings => "Ajustes",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Search => "search",
            Self::Library => "music",
            Self::Playlists => "playlist",
            Self::Stems => "scissors",
            Self::Downloads => "download",
            Self::Settings => "settings",
        }
    }
}

pub struct Services {
    pub core: AppCore,
    pub audio: Result<AudioPlayer, String>,
    pub youtube: YoutubeClient,
    pub downloader: Downloader,
    pub profile: PathBuf,
    pub library_dir: PathBuf,
    download_slots: Arc<DownloadLimiter>,
}

/// A semaphore bounds all downloads while the adjustable limit admits new
/// work only after active jobs fall below the current preference.
struct DownloadLimiter {
    semaphore: Arc<Semaphore>,
    admission: Mutex<()>,
    limit: AtomicUsize,
    changed: Notify,
}

struct DownloadSlot {
    permit: Option<OwnedSemaphorePermit>,
    limiter: Arc<DownloadLimiter>,
}

impl Drop for DownloadSlot {
    fn drop(&mut self) {
        drop(self.permit.take());
        self.limiter.changed.notify_waiters();
    }
}

impl DownloadLimiter {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            semaphore: Arc::new(Semaphore::new(8)),
            admission: Mutex::new(()),
            limit: AtomicUsize::new(limit.clamp(1, 8)),
            changed: Notify::new(),
        })
    }

    fn set_limit(&self, limit: usize) {
        self.limit.store(limit.clamp(1, 8), Ordering::Release);
        self.changed.notify_waiters();
    }

    async fn acquire(self: &Arc<Self>, cancel: &CancellationToken) -> Result<DownloadSlot, String> {
        loop {
            if cancel.is_cancelled() {
                return Err("La operación fue cancelada".into());
            }
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let _admission = self
                    .admission
                    .lock()
                    .map_err(|_| "No se pudo acceder a la cola de descargas")?;
                if 8 - self.semaphore.available_permits() < self.limit.load(Ordering::Acquire) {
                    if let Ok(permit) = self.semaphore.clone().try_acquire_owned() {
                        return Ok(DownloadSlot {
                            permit: Some(permit),
                            limiter: self.clone(),
                        });
                    }
                }
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err("La operación fue cancelada".into()),
                _ = notified => {},
            }
        }
    }
}

fn desktop_profile(home: &Path, configuration: &Path, macos: bool) -> PathBuf {
    if macos {
        home.join("Library/Application Support/YT Ultimate")
    } else {
        configuration.join("YT Ultimate")
    }
}

impl Services {
    #[cfg(test)]
    pub(crate) fn for_test(profile: &Path) -> Self {
        let core = AppCore::open(profile).expect("temporary profile");
        Self {
            library_dir: core.library_dir().to_owned(),
            profile: profile.to_owned(),
            core,
            audio: Err("Audio output disabled for tests".into()),
            youtube: YoutubeClient::new().expect("YouTube client"),
            downloader: Downloader::new().expect("download client"),
            download_slots: DownloadLimiter::new(3),
        }
    }
    pub fn open() -> Result<Self, String> {
        let base = directories::BaseDirs::new().ok_or("No se encontró la carpeta de usuario")?;
        let profile = std::env::var_os("YTU_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                #[cfg(target_os = "windows")]
                let configuration = std::env::var_os("APPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| base.config_dir().to_owned());
                #[cfg(not(target_os = "windows"))]
                let configuration = base.config_dir().to_owned();
                desktop_profile(base.home_dir(), &configuration, cfg!(target_os = "macos"))
            });
        let library_dir = std::env::var_os("YTU_LIBRARY_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let legacy = base.home_dir().join("Downloads/YT_Ultimate");
                if legacy.is_dir() {
                    legacy
                } else {
                    base.home_dir().join("Music/YT Ultimate")
                }
            });
        let core = AppCore::open_with_library(&profile, &library_dir).map_err(|e| e.to_string())?;
        let concurrency = core
            .settings()
            .map_err(|e| e.to_string())?
            .max_concurrent_downloads;
        Ok(Self {
            core,
            audio: AudioPlayer::new().map_err(|e| e.to_string()),
            youtube: YoutubeClient::new().map_err(|e| e.to_string())?,
            downloader: Downloader::new().map_err(|e| e.to_string())?,
            profile,
            library_dir,
            download_slots: DownloadLimiter::new(concurrency as usize),
        })
    }
}

#[derive(Clone, PartialEq)]
pub struct DownloadView {
    pub id: String,
    pub title: String,
    pub progress: f64,
    pub status: String,
    pub detail: String,
    pub source: String,
    pub video: bool,
}

#[derive(Clone)]
pub struct AppState {
    pub services: Rc<Services>,
    pub page: Signal<Page>,
    pub library: Signal<Vec<Track>>,
    pub playlists: Signal<Vec<Playlist>>,
    pub settings: Signal<Settings>,
    pub current: Signal<Option<Track>>,
    pub video_track: Signal<Option<Track>>,
    pub history: Signal<Vec<Track>>,
    pub queue: Signal<Vec<Track>>,
    pub playing: Signal<bool>,
    pub position: Signal<f64>,
    pub duration: Signal<f64>,
    pub volume: Signal<f32>,
    pub rate: Signal<f32>,
    pub shuffle: Signal<bool>,
    pub repeat: Signal<u8>,
    pub queue_open: Signal<bool>,
    pub options_open: Signal<bool>,
    pub menu_track: Signal<Option<Track>>,
    pub notice: Signal<Option<String>>,
    pub busy: Signal<bool>,
    pub downloads: Signal<Vec<DownloadView>>,
    pub cancellations: Signal<HashMap<String, CancellationToken>>,
    pub sleep_at: Signal<Option<Instant>>,
    last_saved_playback: Rc<RefCell<Option<PlaybackState>>>,
}

fn restore_playback(core: &AppCore, state: &PlaybackState) -> (Option<Track>, Vec<Track>) {
    let current_id = state.current_index.and_then(|index| state.queue.get(index));
    let current = current_id
        .and_then(|id| core.track(id).ok())
        .filter(|track| !track.missing);
    let queue = state
        .queue
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != state.current_index)
        .filter_map(|(_, id)| core.track(id).ok())
        .filter(|track| !track.missing)
        .collect();
    (current, queue)
}

fn download_view(job: &Job) -> DownloadView {
    DownloadView {
        id: job.id.clone(),
        title: job.payload["title"].as_str().unwrap_or("Descarga").into(),
        progress: job.progress,
        status: match job.status {
            JobStatus::Pending => "Preparando",
            JobStatus::Running => "Descargando",
            JobStatus::Completed => "Completado",
            JobStatus::Error => "Error",
            JobStatus::Canceled => "Cancelado",
            JobStatus::Interrupted => "Interrumpido",
        }
        .into(),
        detail: if job.error.is_empty() {
            job.message.clone()
        } else {
            job.error.clone()
        },
        source: job.payload["url"].as_str().unwrap_or_default().into(),
        video: job.payload["video"]
            .as_bool()
            .unwrap_or_else(|| job.payload["type"].as_str() == Some("video")),
    }
}

fn save_playback_if_changed(
    core: &AppCore,
    previous: &RefCell<Option<PlaybackState>>,
    state: PlaybackState,
) -> yt_core::Result<bool> {
    if previous.borrow().as_ref() == Some(&state) {
        return Ok(false);
    }
    core.save_playback(&state)?;
    *previous.borrow_mut() = Some(state);
    Ok(true)
}

pub fn use_app_state(services: Rc<Services>) -> AppState {
    let core = &services.core;
    let initial_result = use_hook(|| core.load_playback().map_err(|e| e.to_string()));
    let initial = initial_result.clone().unwrap_or_default();
    let restored = use_hook(|| restore_playback(core, &initial));
    let library = use_signal(|| core.library("").unwrap_or_default());
    let playlists = use_signal(|| core.playlists().unwrap_or_default());
    let settings = use_signal(|| core.settings().unwrap_or_default());
    let history = use_signal(|| {
        let mut seen = std::collections::HashSet::new();
        core.history(50)
            .unwrap_or_default()
            .into_iter()
            .map(|h| h.track)
            .filter(|track| seen.insert(track.id.clone()) && !track.missing)
            .collect()
    });
    let queue = use_signal(|| restored.1.clone());
    AppState {
        services: services.clone(),
        page: use_signal(|| Page::Home),
        library,
        playlists,
        settings,
        current: use_signal(|| restored.0.clone()),
        video_track: use_signal(|| None),
        history,
        queue,
        playing: use_signal(|| false),
        position: use_signal(|| {
            if restored.0.is_some() {
                initial.position_seconds
            } else {
                0.0
            }
        }),
        duration: use_signal(|| restored.0.as_ref().map(|t| t.duration).unwrap_or(0.0)),
        volume: use_signal(|| {
            if initial.playback_rate > 0.0 {
                initial.volume as f32
            } else {
                0.8
            }
        }),
        rate: use_signal(|| {
            if initial.playback_rate > 0.0 {
                initial.playback_rate as f32
            } else {
                1.0
            }
        }),
        shuffle: use_signal(|| initial.shuffle),
        repeat: use_signal(|| match initial.repeat.as_str() {
            "all" => 1,
            "one" => 2,
            _ => 0,
        }),
        queue_open: use_signal(|| false),
        options_open: use_signal(|| false),
        menu_track: use_signal(|| None),
        notice: use_signal(|| {
            initial_result
                .err()
                .map(|e| format!("No se pudo restaurar la reproducción: {e}"))
        }),
        busy: use_signal(|| false),
        downloads: use_signal(|| {
            core.jobs()
                .unwrap_or_default()
                .iter()
                .filter(|job| matches!(job.kind.as_str(), "download" | "playlist_download"))
                .map(download_view)
                .collect()
        }),
        cancellations: use_signal(HashMap::new),
        sleep_at: use_signal(|| None),
        last_saved_playback: use_hook(|| Rc::new(RefCell::new(None))),
    }
}

impl AppState {
    pub fn notify(&self, text: impl Into<String>) {
        let mut notice = self.notice;
        notice.set(Some(text.into()));
    }

    pub fn refresh(&self) {
        let mut library = self.library;
        let mut playlists = self.playlists;
        match self.services.core.library("") {
            Ok(v) => library.set(v),
            Err(e) => self.notify(e.to_string()),
        }
        match self.services.core.playlists() {
            Ok(v) => playlists.set(v),
            Err(e) => self.notify(e.to_string()),
        }
    }

    pub fn play(&self, track: Track) {
        if track.media_type == "video" {
            if let Ok(audio) = &self.services.audio {
                if let Err(error) = audio.stop() {
                    self.notify(error.to_string());
                    return;
                }
            }
            let mut state = self.clone();
            state.playing.set(false);
            state.position.set(0.0);
            state.duration.set(track.duration);
            state.current.set(Some(track.clone()));
            state.video_track.set(Some(track.clone()));
            state
                .history
                .write()
                .retain(|previous| previous.id != track.id);
            state.history.write().insert(0, track.clone());
            if let Err(error) = state.services.core.record_playback(&track.id, 0.0) {
                state.notify(error.to_string());
            }
            state.persist_playback();
            return;
        }
        let mut video = self.video_track;
        video.set(None);
        self.play_at(track, 0.0, true);
    }

    fn play_at(&self, track: Track, position: f64, record: bool) {
        let position = if track.duration > 0.0 {
            position.clamp(0.0, track.duration)
        } else {
            position.max(0.0)
        };
        let volume = *self.volume.peek();
        let rate = *self.rate.peek();
        let result = self
            .services
            .audio
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|audio| {
                audio.set_volume(volume).map_err(|e| e.to_string())?;
                audio.set_speed(rate).map_err(|e| e.to_string())?;
                audio.play(&track.path).map_err(|e| e.to_string())?;
                if position > 0.0 {
                    if let Err(error) = audio.seek(position) {
                        let _ = audio.stop();
                        return Err(error.to_string());
                    }
                }
                Ok(())
            });
        match result {
            Ok(()) => {
                let mut s = self.clone();
                s.duration.set(track.duration);
                s.position.set(position);
                s.playing.set(true);
                s.current.set(Some(track.clone()));
                s.history.write().retain(|t| t.id != track.id);
                s.history.write().insert(0, track.clone());
                if record {
                    if let Err(e) = s.services.core.record_playback(&track.id, position) {
                        s.notify(e.to_string());
                    }
                }
                s.persist_playback();
            }
            Err(error) => {
                let previous_path = self.current.peek().as_ref().map(|track| track.path.clone());
                if self
                    .services
                    .audio
                    .as_ref()
                    .is_ok_and(|audio| audio.snapshot().path != previous_path)
                {
                    let mut playing = self.playing;
                    playing.set(false);
                }
                self.notify(format!("No se pudo reproducir: {error}"));
            }
        }
    }

    pub fn toggle_play(&self) {
        let current = self.current.peek().clone();
        let Some(track) = current else {
            return;
        };
        if track.media_type == "video" {
            let mut video = self.video_track;
            video.set(Some(track));
            return;
        }
        let Ok(audio) = &self.services.audio else {
            self.play(track);
            return;
        };
        let mut s = self.clone();
        let was_playing = *s.playing.read();
        if audio.snapshot().path.is_none() {
            let position = *self.position.peek();
            self.play_at(track, position, false);
            return;
        }
        if audio.snapshot().finished {
            self.play(track);
            return;
        }
        let result = if was_playing {
            audio.pause()
        } else {
            audio.resume()
        };
        match result {
            Ok(()) => {
                s.playing.set(!was_playing);
                s.persist_playback();
            }
            Err(e) => s.notify(e.to_string()),
        }
    }

    pub fn play_all(&self, mut tracks: Vec<Track>, shuffled: bool) {
        if shuffled {
            use rand::seq::SliceRandom;
            tracks.shuffle(&mut rand::rng());
        }
        if tracks.is_empty() {
            return;
        }
        let first = tracks.remove(0);
        let mut queue = self.queue;
        queue.set(tracks);
        self.play(first);
    }

    pub fn next(&self, automatic: bool) {
        let repeat = *self.repeat.peek();
        if automatic && repeat == 2 {
            let current = self.current.peek().clone();
            if let Some(track) = current {
                self.play(track);
            }
            return;
        }
        let mut tracks = self.queue.read().clone();
        if repeat == 1 {
            let current = self.current.peek().clone();
            if let Some(current) = current {
                tracks.push(current);
            }
        }
        if tracks.is_empty() {
            if let Ok(audio) = &self.services.audio {
                if let Err(error) = audio.pause() {
                    self.notify(error.to_string());
                }
            }
            let mut playing = self.playing;
            playing.set(false);
            self.persist_playback();
            return;
        }
        let index = if *self.shuffle.read() {
            rand::random_range(0..tracks.len())
        } else {
            0
        };
        let track = tracks.remove(index);
        let mut queue = self.queue;
        queue.set(tracks);
        self.play(track);
    }

    pub fn previous(&self) {
        let position = *self.position.peek();
        let current = self.current.peek().clone();
        let is_video = current
            .as_ref()
            .is_some_and(|track| track.media_type == "video");
        if position > 3.0 && !is_video {
            self.seek(0.0);
            return;
        }
        let current_id = current.map(|v| v.id);
        let previous = self
            .history
            .read()
            .iter()
            .find(|v| Some(&v.id) != current_id.as_ref())
            .cloned();
        if let Some(track) = previous {
            self.play(track);
        }
    }

    pub fn seek(&self, seconds: f64) {
        let is_video = self
            .current
            .peek()
            .as_ref()
            .is_some_and(|track| track.media_type == "video");
        if is_video {
            self.notify("Usa los controles del vídeo para cambiar la posición");
            return;
        }
        if !seconds.is_finite() {
            self.notify("La posición de reproducción no es válida");
            return;
        }
        let duration = *self.duration.peek();
        let seconds = if duration > 0.0 {
            seconds.clamp(0.0, duration)
        } else {
            seconds.max(0.0)
        };
        if let Ok(audio) = &self.services.audio {
            if audio.snapshot().path.is_none() {
                let mut position = self.position;
                position.set(seconds);
                self.persist_playback();
                return;
            }
            match audio.seek(seconds) {
                Ok(()) => {
                    let mut p = self.position;
                    p.set(seconds);
                    self.persist_playback();
                }
                Err(e) => self.notify(e.to_string()),
            }
        }
    }

    pub fn set_volume(&self, value: f32) {
        if !value.is_finite() {
            self.notify("El volumen no es válido");
            return;
        }
        let value = value.clamp(0.0, 1.0);
        let mut volume = self.volume;
        volume.set(value);
        if let Ok(audio) = &self.services.audio {
            if let Err(e) = audio.set_volume(value) {
                self.notify(e.to_string());
            }
        }
        self.persist_playback();
    }

    pub fn set_rate(&self, value: f32) {
        if let Ok(audio) = &self.services.audio {
            match audio.set_speed(value) {
                Ok(()) => {
                    let mut rate = self.rate;
                    rate.set(value);
                    self.persist_playback();
                }
                Err(e) => self.notify(e.to_string()),
            }
        }
    }

    pub fn toggle_shuffle(&self) {
        let value = !*self.shuffle.peek();
        let mut shuffle = self.shuffle;
        shuffle.set(value);
        self.persist_playback();
    }

    pub fn cycle_repeat(&self) {
        let value = (*self.repeat.peek() + 1) % 3;
        let mut repeat = self.repeat;
        repeat.set(value);
        self.persist_playback();
    }

    pub fn persist_playback(&self) {
        let mut queue: Vec<String> = self.queue.peek().iter().map(|t| t.id.clone()).collect();
        let current_index = self.current.peek().as_ref().map(|track| {
            queue.insert(0, track.id.clone());
            0
        });
        let state = PlaybackState {
            queue,
            current_index,
            position_seconds: if current_index.is_some() {
                *self.position.peek()
            } else {
                0.0
            },
            volume: *self.volume.peek() as f64,
            playback_rate: *self.rate.peek() as f64,
            shuffle: *self.shuffle.peek(),
            repeat: match *self.repeat.peek() {
                1 => "all",
                2 => "one",
                _ => "off",
            }
            .into(),
        };
        match save_playback_if_changed(&self.services.core, &self.last_saved_playback, state) {
            Ok(_) => {}
            Err(e) => self.notify(format!("No se pudo guardar la reproducción: {e}")),
        }
    }

    pub fn save_settings(&self, value: Settings) {
        match self.services.core.save_settings(&value) {
            Ok(()) => {
                self.services
                    .download_slots
                    .set_limit(value.max_concurrent_downloads as usize);
                let mut settings = self.settings;
                settings.set(value);
            }
            Err(e) => self.notify(format!("No se guardaron los ajustes: {e}")),
        }
    }

    pub fn import_files(&self, folder: bool) {
        let s = self.clone();
        dioxus::core::spawn_forever(async move {
            let dialog = rfd::AsyncFileDialog::new().set_title("Importar música y vídeo");
            let paths = if folder {
                dialog
                    .pick_folder()
                    .await
                    .map(|p| vec![p.path().to_path_buf()])
                    .unwrap_or_default()
            } else {
                dialog
                    .add_filter(
                        "Audio, vídeo y playlists",
                        &[
                            "mp3", "mp4", "m4a", "webm", "mkv", "wav", "flac", "aac", "ogg",
                            "opus", "m3u", "m3u8",
                        ],
                    )
                    .pick_files()
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|p| p.path().to_path_buf())
                    .collect()
            };
            if paths.is_empty() {
                return;
            }
            s.import_paths(paths).await;
        });
    }

    pub fn export_audio(&self, track: Track, format: yt_audio::AudioFormat) {
        let mut state = self.clone();
        dioxus::core::spawn_forever(async move {
            let name = yt_media::safe_file_name(&track.title, format.extension());
            let Some(output) = rfd::AsyncFileDialog::new()
                .set_title("Exportar audio")
                .set_file_name(name)
                .add_filter("Audio", &[format.extension()])
                .save_file()
                .await
            else {
                return;
            };
            let output = output.path().to_owned();
            let cancel = CancellationToken::new();
            state.busy.set(true);
            state.notify("Exportando audio…");
            let result = tokio::task::spawn_blocking(move || {
                yt_audio::convert(&track.path, &output, format, &cancel, |_| {})
            })
            .await;
            state.busy.set(false);
            match result {
                Ok(Ok(path)) => state.notify(format!("Audio exportado: {}", path.display())),
                Ok(Err(error)) => state.notify(format!("No se pudo exportar: {error}")),
                Err(error) => state.notify(format!("No se pudo exportar: {error}")),
            }
        });
    }

    pub async fn import_paths(&self, paths: Vec<PathBuf>) {
        let mut busy = self.busy;
        busy.set(true);
        let core = self.services.core.clone();
        let mode = if self.settings.read().import_mode == "reference" {
            ImportMode::Reference
        } else {
            ImportMode::Copy
        };
        let result = tokio::task::spawn_blocking(move || core.import_paths(&paths, mode)).await;
        busy.set(false);
        match result {
            Ok(Ok(report)) => {
                self.refresh();
                let errors = report
                    .errors
                    .iter()
                    .take(3)
                    .map(|e| e.message.clone())
                    .collect::<Vec<_>>()
                    .join(" · ");
                self.notify(if report.errors.is_empty() {
                    format!(
                        "{} archivos importados{}",
                        report.tracks.len(),
                        if report.playlists.is_empty() {
                            String::new()
                        } else {
                            format!(" · {} playlists", report.playlists.len())
                        }
                    )
                } else {
                    format!(
                        "{} importados; {} errores: {errors}",
                        report.tracks.len(),
                        report.errors.len()
                    )
                });
            }
            Ok(Err(e)) => self.notify(e.to_string()),
            Err(e) => self.notify(e.to_string()),
        }
    }

    fn save_download_progress(
        &self,
        id: &str,
        status: JobStatus,
        progress: f64,
        message: &str,
        result: &serde_json::Value,
        error: &str,
    ) -> Result<(), String> {
        let job = self
            .services
            .core
            .update_job(id, status, progress, message, result, error)
            .map_err(|e| e.to_string())?;
        let view = download_view(&job);
        let mut downloads = self.downloads;
        if let Some(row) = downloads.write().iter_mut().find(|row| row.id == id) {
            *row = view;
        }
        Ok(())
    }

    pub fn download(&self, item: SearchResult, video: bool, play_when_ready: bool) {
        self.download_into_playlist(item, video, play_when_ready, None, None);
    }

    pub fn download_playlist(&self, name: &str, items: Vec<SearchResult>, video: bool) {
        let name = name
            .chars()
            .map(|c| {
                if c.is_control() || "/\\".contains(c) {
                    ' '
                } else {
                    c
                }
            })
            .take(240)
            .collect::<String>();
        let name = if name.trim().is_empty() {
            "Playlist"
        } else {
            name.trim()
        };
        let playlist = self.services.core.playlists().and_then(|list| {
            match list.into_iter().find(|p| p.name.eq_ignore_ascii_case(name)) {
                Some(p) => Ok(p),
                None => self.services.core.create_playlist(name),
            }
        });
        let playlist = match playlist {
            Ok(p) => p,
            Err(e) => {
                self.notify(e.to_string());
                return;
            }
        };
        let count = items.len();
        let order = std::sync::Arc::new(
            items
                .iter()
                .map(|item| item.url.clone())
                .collect::<Vec<_>>(),
        );
        for item in items {
            self.download_into_playlist(
                item,
                video,
                false,
                Some(playlist.id.clone()),
                Some(order.clone()),
            );
        }
        self.refresh();
        self.notify(format!("{count} descargas añadidas a {}", playlist.name));
    }

    pub fn retry_download(&self, id: &str) {
        let Ok(job) = self.services.core.job(id) else {
            return;
        };
        if !job.status.is_terminal() {
            return;
        }
        let value = |key: &str| job.payload[key].as_str().unwrap_or_default().to_owned();
        self.download_into_playlist(
            SearchResult {
                id: String::new(),
                url: value("url"),
                title: value("title"),
                thumbnail: value("thumbnail"),
                channel: value("channel"),
                duration_seconds: 0,
                view_count: 0,
            },
            job.payload["video"].as_bool().unwrap_or(false),
            false,
            job.payload["playlist_id"].as_str().map(str::to_owned),
            None,
        );
    }

    fn download_into_playlist(
        &self,
        item: SearchResult,
        video: bool,
        play_when_ready: bool,
        playlist: Option<String>,
        order: Option<std::sync::Arc<Vec<String>>>,
    ) {
        let mut s = self.clone();
        let payload = serde_json::json!({"url": item.url, "title": item.title, "video": video, "thumbnail": item.thumbnail, "channel": item.channel,"playlist_id":playlist});
        let job = match s.services.core.create_job("download", &payload) {
            Ok(job) => job,
            Err(error) => {
                s.notify(format!("No se pudo iniciar la descarga: {error}"));
                return;
            }
        };
        let id = job.id.clone();
        let cancel = CancellationToken::new();
        s.cancellations.write().insert(id.clone(), cancel.clone());
        s.downloads.write().insert(0, download_view(&job));
        dioxus::core::spawn_forever(async move {
            let result = async {
                let _slot = s.services.download_slots.acquire(&cancel).await?;
                s.save_download_progress(&id,JobStatus::Running,0.0,"Preparando descarga",&serde_json::json!({}),"")?;
                // Replaying a previously downloaded source reuses its local copy.
                let existing = s.services.core.library("").map_err(|e|e.to_string())?.into_iter()
                    .find(|track| !track.missing && track.source_url == item.url && (track.media_type == "video") == video);
                if let Some(track) = existing { return Ok(track); }
                let kind = if video { StreamKind::Video } else { StreamKind::Audio };
                let resolved = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err("La operación fue cancelada".to_owned()),
                    resolved = s.services.youtube.resolve(&item.url,kind) => resolved.map_err(|e|e.to_string())?,
                };
                let name = yt_media::safe_file_name(&resolved.media.title, &resolved.extension);
                let source_cache = tempfile::tempdir().map_err(|e| e.to_string())?;
                // Each job publishes a unique destination without replacing media.
                let destination = if video { s.services.library_dir.join(format!("{id}-{name}")) } else { source_cache.path().join(&name) };
                let (tx, mut rx) = tokio::sync::mpsc::channel(16);
                let retries=s.settings.read().download_retries;
                let retry_state=s.clone();
                let download = async {
                    let mut attempt=0u8;
                    loop {
                        let updates=tx.clone();
                        let result=s.services.downloader.download_sized(&resolved.stream_url,&destination,resolved.content_length,cancel.clone(),move|progress|{let _=updates.try_send(progress);}).await;
                        match result {
                            Err(error) if attempt<retries && matches!(error,yt_media::MediaError::Network(_)|yt_media::MediaError::IncompleteDownload) => {
                                attempt+=1;
                                retry_state.save_download_progress(&id,JobStatus::Running,0.0,&format!("Reintentando descarga ({attempt}/{retries})"),&serde_json::json!({}),"").map_err(yt_media::MediaError::Network)?;
                                tokio::select!{_ = cancel.cancelled()=>return Err(yt_media::MediaError::Cancelled),_=tokio::time::sleep(Duration::from_secs((attempt as u64).min(5)))=>{}}
                            },
                            result=>return result,
                        }
                    }
                };
                tokio::pin!(download);
                let mut last_saved = Instant::now();
                let receipt = loop {
                    tokio::select! {
                        result = &mut download => break result.map_err(|e| e.to_string())?,
                        Some(progress) = rx.recv() => {
                            let percent = progress.fraction().map(|fraction|fraction*100.0).unwrap_or(0.0).min(99.9);
                            let detail = format!("{:.1} MB", progress.downloaded_bytes as f64 / 1_048_576.0);
                            if let Some(row) = s.downloads.write().iter_mut().find(|r| r.id == id) {
                                row.status = "Descargando".into(); row.progress = percent; row.detail = detail.clone();
                            }
                            if last_saved.elapsed() >= Duration::from_secs(1) {
                                s.save_download_progress(&id,JobStatus::Running,percent,&detail,&serde_json::json!({}),"")?;
                                last_saved=Instant::now();
                            }
                        }
                    }
                };
                let published_path = if video { receipt.path } else {
                    s.save_download_progress(&id,JobStatus::Running,99.9,"Convirtiendo a MP3",&serde_json::json!({}),"")?;
                    let quality = s.settings.read().audio_quality.parse::<u16>().unwrap_or(320);
                    let output = s.services.library_dir.join(format!("{id}-{}", yt_media::safe_file_name(&resolved.media.title, "mp3")));
                    let cancel_conversion = cancel.clone();
                    tokio::task::spawn_blocking(move || yt_audio::convert(&receipt.path, &output, yt_audio::AudioFormat::Mp3(quality), &cancel_conversion, |_|{}))
                        .await.map_err(|e|e.to_string())?.map_err(|e|e.to_string())?
                };
                // Once the complete file has been atomically published, finish
                // indexing it even if cancellation arrives at this final step.
                let core = s.services.core.clone();
                let source_url=item.url.clone();
                let track = tokio::task::spawn_blocking(move || {
                    let track=core.import_file(&published_path,ImportMode::Reference)?;
                    core.update_track_metadata(&track.id,&resolved.media.title,&resolved.media.channel,&resolved.media.thumbnail,&source_url)
                }).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
                Ok::<_, String>(track)
            }.await;
            s.cancellations.write().remove(&id);
            match result {
                Ok(track) => {
                    if let Some(playlist) = &playlist {
                        if let Err(error) = s.services.core.add_to_playlist(playlist, &track.id) {
                            s.notify(format!("El archivo se guardó, pero no se pudo añadir a la playlist: {error}"));
                        } else if let Some(order) = &order {
                            let result =
                                s.services
                                    .core
                                    .playlist_tracks(playlist)
                                    .and_then(|mut tracks| {
                                        tracks.sort_by_key(|track| {
                                            order
                                                .iter()
                                                .position(|url| *url == track.source_url)
                                                .map(|index| (1, index))
                                                .unwrap_or((0, 0))
                                        });
                                        s.services.core.reorder_playlist(
                                            playlist,
                                            &tracks
                                                .into_iter()
                                                .map(|track| track.id)
                                                .collect::<Vec<_>>(),
                                        )
                                    });
                            if let Err(error) = result {
                                s.notify(format!(
                                    "La playlist se guardó, pero no se pudo ordenar: {error}"
                                ));
                            }
                        }
                    }
                    if let Err(error) = s.save_download_progress(
                        &id,
                        JobStatus::Completed,
                        100.0,
                        "Descarga completada",
                        &serde_json::json!({"track_id":track.id}),
                        "",
                    ) {
                        s.notify(format!("El archivo se guardó, pero no se pudo actualizar su historial: {error}"));
                    }
                    s.refresh();
                    if play_when_ready {
                        s.play(track);
                    }
                }
                Err(error) => {
                    let status = if cancel.is_cancelled() {
                        JobStatus::Canceled
                    } else {
                        JobStatus::Error
                    };
                    let progress = s
                        .downloads
                        .peek()
                        .iter()
                        .find(|row| row.id == id)
                        .map(|row| row.progress)
                        .unwrap_or(0.0);
                    if let Err(persistence_error) = s.save_download_progress(
                        &id,
                        status,
                        progress,
                        &error,
                        &serde_json::json!({}),
                        if status == JobStatus::Canceled {
                            ""
                        } else {
                            &error
                        },
                    ) {
                        s.notify(format!(
                            "No se pudo guardar el estado de la descarga: {persistence_error}"
                        ));
                    } else if !cancel.is_cancelled() {
                        s.notify(format!("No se pudo descargar: {error}"));
                    }
                }
            }
        });
    }

    pub fn start_polling(&self) {
        if self.settings.peek().scan_library_on_startup {
            let state = self.clone();
            dioxus::core::spawn_forever(async move {
                let core = state.services.core.clone();
                match tokio::task::spawn_blocking(move || core.scan_library()).await {
                    Ok(Ok(report)) => {
                        state.refresh();
                        if !report.errors.is_empty() {
                            state.notify(format!("La revisión de la biblioteca encontró {} archivos que no se pudieron importar",report.errors.len()));
                        }
                    }
                    Ok(Err(error)) => {
                        state.notify(format!("No se pudo revisar la biblioteca: {error}"))
                    }
                    Err(error) => state.notify(format!(
                        "La revisión de la biblioteca se interrumpió: {error}"
                    )),
                }
            });
        }
        let mut s = self.clone();
        dioxus::core::spawn_forever(async move {
            let mut ticks = 0u8;
            let mut last_audio_error = None;
            loop {
                tokio::time::sleep(Duration::from_millis(250)).await;
                if let Ok(audio) = &s.services.audio {
                    let snapshot = audio.snapshot();
                    let current_is_audio = s.current.peek().as_ref().is_some_and(|track| {
                        track.media_type != "video" && snapshot.path.as_ref() == Some(&track.path)
                    });
                    if current_is_audio {
                        let old_position = *s.position.peek();
                        let old_duration = *s.duration.peek();
                        let was_playing = *s.playing.peek();
                        if old_position != snapshot.position_seconds {
                            s.position.set(snapshot.position_seconds);
                        }
                        if snapshot.duration_seconds > 0.0
                            && old_duration != snapshot.duration_seconds
                        {
                            s.duration.set(snapshot.duration_seconds);
                        }
                        if let Some(error) = &snapshot.error {
                            if last_audio_error.as_ref() != Some(error) {
                                s.notify(error.clone());
                            }
                            s.playing.set(false);
                        } else if was_playing && snapshot.finished {
                            s.next(true);
                        }
                        last_audio_error = snapshot.error;
                    }
                }
                let timer_expired = s.sleep_at.peek().is_some_and(|at| at <= Instant::now());
                if timer_expired {
                    s.expire_sleep_timer();
                }
                ticks += 1;
                if ticks == 20 {
                    ticks = 0;
                    s.persist_playback();
                }
            }
        });
    }

    fn expire_sleep_timer(&self) {
        let pause_error = self
            .services
            .audio
            .as_ref()
            .ok()
            .and_then(|audio| audio.pause().err());
        let mut state = self.clone();
        state.video_track.set(None);
        state.playing.set(false);
        state.sleep_at.set(None);
        state.persist_playback();
        match pause_error {
            Some(error) => state.notify(format!(
                "El temporizador terminó, pero no se pudo pausar el audio: {error}"
            )),
            None => state.notify("Temporizador finalizado"),
        }
    }
}

pub fn time_label(seconds: f64) -> String {
    let seconds = if seconds.is_finite() {
        seconds.max(0.0) as u64
    } else {
        0
    };
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

pub fn size_label(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_048_576.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_timestamps_never_leak_to_ui() {
        assert_eq!(time_label(f64::NAN), "0:00");
        assert_eq!(time_label(-1.0), "0:00");
        assert_eq!(time_label(3661.8), "61:01");
    }

    #[test]
    fn desktop_profiles_match_existing_electron_installations() {
        assert_eq!(
            desktop_profile(Path::new("/Users/user"), Path::new("/ignored"), true),
            Path::new("/Users/user/Library/Application Support/YT Ultimate")
        );
        assert_eq!(
            desktop_profile(
                Path::new("/home/user"),
                Path::new("/Users/user/AppData/Roaming"),
                false
            ),
            Path::new("/Users/user/AppData/Roaming/YT Ultimate")
        );
    }

    #[test]
    fn playback_restore_keeps_current_identity_when_old_queue_files_are_missing() {
        let profile = tempfile::TempDir::new().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let current = core.import_bytes("current.wav", &[1, 2, 3, 4]).unwrap();
        let upcoming = core.import_bytes("upcoming.wav", &[1, 2, 3, 4]).unwrap();
        let state = PlaybackState {
            queue: vec!["missing".into(), current.id.clone(), upcoming.id.clone()],
            current_index: Some(1),
            position_seconds: 12.5,
            ..Default::default()
        };
        core.save_playback(&state).unwrap();
        let restored = restore_playback(&core, &core.load_playback().unwrap());
        assert_eq!(restored.0.unwrap().id, current.id);
        assert_eq!(
            restored.1.iter().map(|track| &track.id).collect::<Vec<_>>(),
            vec![&upcoming.id]
        );
        assert_eq!(core.load_playback().unwrap().position_seconds, 12.5);
    }

    #[test]
    fn identical_playback_is_not_written_and_failed_saves_remain_retryable() {
        let profile = tempfile::TempDir::new().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let cache = RefCell::new(None);
        let mut state = PlaybackState::default();
        assert!(save_playback_if_changed(&core, &cache, state.clone()).unwrap());
        assert!(!save_playback_if_changed(&core, &cache, state.clone()).unwrap());
        state.volume = 0.25;
        assert!(save_playback_if_changed(&core, &cache, state.clone()).unwrap());
        assert_eq!(core.load_playback().unwrap().volume, 0.25);
        state.playback_rate = 100.0;
        assert!(save_playback_if_changed(&core, &cache, state).is_err());
        assert_eq!(cache.borrow().as_ref().unwrap().playback_rate, 1.0);
    }

    #[test]
    fn interrupted_download_history_is_visible_after_restart() {
        let profile = tempfile::TempDir::new().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let job=core.create_job("download",&serde_json::json!({"url":"https://www.youtube.com/watch?v=abcdefghijk","title":"A track","video":true})).unwrap();
        core.update_job(
            &job.id,
            JobStatus::Running,
            42.0,
            "Downloading",
            &serde_json::json!({}),
            "",
        )
        .unwrap();
        drop(core);
        let reopened = AppCore::open(profile.path()).unwrap();
        let row = download_view(&reopened.job(&job.id).unwrap());
        assert_eq!(row.status, "Interrumpido");
        assert_eq!(row.progress, 42.0);
        assert_eq!(row.title, "A track");
        assert!(row.video);
    }

    #[test]
    fn download_concurrency_tracks_live_limit_and_cancellation() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let limiter = DownloadLimiter::new(2);
                let cancel = CancellationToken::new();
                let first = limiter.acquire(&cancel).await.unwrap();
                let second = limiter.acquire(&cancel).await.unwrap();
                assert!(
                    tokio::time::timeout(Duration::from_millis(15), limiter.acquire(&cancel))
                        .await
                        .is_err()
                );
                limiter.set_limit(1);
                drop(first);
                assert!(
                    tokio::time::timeout(Duration::from_millis(15), limiter.acquire(&cancel))
                        .await
                        .is_err()
                );
                drop(second);
                let third = limiter.acquire(&cancel).await.unwrap();
                limiter.set_limit(2);
                let fourth = limiter.acquire(&cancel).await.unwrap();
                let waiting_cancel = CancellationToken::new();
                let task_limiter = limiter.clone();
                let task_cancel = waiting_cancel.clone();
                let task = tokio::spawn(async move { task_limiter.acquire(&task_cancel).await });
                tokio::task::yield_now().await;
                waiting_cancel.cancel();
                assert!(tokio::time::timeout(Duration::from_secs(1), task)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_err());
                drop(third);
                drop(fourth);
                assert_eq!(limiter.semaphore.available_permits(), 8);
            });
    }

    type CapturedState = Rc<RefCell<Option<AppState>>>;

    fn action_harness() -> Element {
        let services = use_context::<Rc<Services>>();
        let capture = use_context::<CapturedState>();
        let state = use_app_state(services);
        *capture.borrow_mut() = Some(state);
        rsx! { div { "Action test" } }
    }

    #[tokio::test]
    async fn virtual_dom_playback_actions_release_signal_borrows_before_mutation() {
        let profile = tempfile::TempDir::new().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        // Video actions exercise the actual successful state transitions
        // without opening an audio device or a native video window.
        let first = services
            .core
            .import_bytes("first.mp4", &[1, 2, 3, 4])
            .unwrap();
        let second = services
            .core
            .import_bytes("second.mp4", &[1, 2, 3, 4])
            .unwrap();
        let third = services
            .core
            .import_bytes("third.mp4", &[1, 2, 3, 4])
            .unwrap();
        services
            .core
            .save_playback(&PlaybackState {
                queue: vec![first.id.clone(), second.id.clone()],
                current_index: Some(0),
                position_seconds: 12.5,
                ..Default::default()
            })
            .unwrap();
        let capture: CapturedState = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(action_harness);
        dom.insert_any_root_context(Box::new(services.clone()));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let mut state = capture.borrow().as_ref().unwrap().clone();
        dom.in_scope(ScopeId::ROOT, || {
            assert_eq!(state.current.peek().as_ref().unwrap().id, first.id);
            assert_eq!(*state.position.peek(), 12.5);
            assert!(!*state.playing.peek());
            state.repeat.set(2);
            state.next(true);
            assert_eq!(state.current.peek().as_ref().unwrap().id, first.id);
            state.repeat.set(0);
            state.next(false);
            assert_eq!(state.current.peek().as_ref().unwrap().id, second.id);
            state.history.set(vec![second.clone(), third.clone()]);
            state.position.set(15.0);
            state.previous();
            assert_eq!(state.current.peek().as_ref().unwrap().id, third.id);
            state.video_track.set(None);
            state.toggle_play();
            assert_eq!(state.video_track.peek().as_ref().unwrap().id, third.id);
            state.seek(30.0);
            assert_eq!(*state.position.peek(), 0.0);
            state.toggle_shuffle();
            state.cycle_repeat();
            state.set_volume(0.4);
            assert!(services.core.load_playback().unwrap().shuffle);
            state.repeat.set(1);
            state.queue.set(Vec::new());
            state.next(false);
            assert_eq!(state.current.peek().as_ref().unwrap().id, third.id);
            state.sleep_at.set(Some(Instant::now()));
            state.expire_sleep_timer();
            assert!(state.video_track.peek().is_none());
            assert!(state.sleep_at.peek().is_none());
            assert!(!*state.playing.peek());
            let audio = services
                .core
                .import_bytes("audio.wav", &[1, 2, 3, 4])
                .unwrap();
            state.current.set(Some(audio.clone()));
            state.toggle_play();
            assert_eq!(state.current.peek().as_ref().unwrap().id, audio.id);
            assert!(state
                .notice
                .peek()
                .as_ref()
                .unwrap()
                .contains("No se pudo reproducir"));
        });
    }
}
