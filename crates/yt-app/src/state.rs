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
use yt_media::{AudioPlayer, CancellationToken, Downloader, YoutubeClient};

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
            Self::Library => "music-2",
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
    youtube: RefCell<YoutubeClient>,
    pub(crate) downloader: RefCell<Downloader>,
    connection_error: RefCell<Option<String>>,
    browser_session: RefCell<Option<yt_media::BrowserCookieSession>>,
    pub profile: PathBuf,
    pub library_dir: PathBuf,
    pub plugins: Result<yt_plugins::PluginHost, String>,
    pub(crate) download_slots: Arc<DownloadLimiter>,
    pub(crate) processing_slots: Arc<DownloadLimiter>,
}

/// A semaphore bounds all downloads while the adjustable limit admits new
/// work only after active jobs fall below the current preference.
pub(crate) struct DownloadLimiter {
    semaphore: Arc<Semaphore>,
    admission: Mutex<()>,
    limit: AtomicUsize,
    changed: Notify,
}

pub(crate) struct DownloadSlot {
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

    pub(crate) async fn acquire(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Result<DownloadSlot, String> {
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

/// Resolve the same profile before startup locking and database initialization.
pub(crate) fn profile_path() -> Result<PathBuf, String> {
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
    Ok(profile)
}

impl Services {
    pub fn youtube_client(&self) -> yt_media::Result<YoutubeClient> {
        if let Some(error) = self.connection_error.borrow().as_ref() {
            return Err(yt_media::MediaError::InvalidInput(error.clone()));
        }
        Ok(self.youtube.borrow().clone())
    }

    pub fn set_browser_session(
        &self,
        session: yt_media::BrowserCookieSession,
    ) -> Result<(), String> {
        let settings = self.core.settings().map_err(|error| error.to_string())?;
        let (youtube, downloader) = Self::connection_clients(&settings, Some(session.clone()))
            .map_err(|error| error.to_string())?;
        *self.youtube.borrow_mut() = youtube;
        *self.downloader.borrow_mut() = downloader;
        *self.browser_session.borrow_mut() = Some(session);
        *self.connection_error.borrow_mut() = None;
        Ok(())
    }
    pub fn clear_browser_session(&self) -> Result<(), String> {
        let settings = self.core.settings().map_err(|error| error.to_string())?;
        let (youtube, downloader) =
            Self::connection_clients(&settings, None).map_err(|error| error.to_string())?;
        *self.youtube.borrow_mut() = youtube;
        *self.downloader.borrow_mut() = downloader;
        *self.browser_session.borrow_mut() = None;
        *self.connection_error.borrow_mut() = None;
        Ok(())
    }
    pub fn browser_session_info(&self) -> Option<(String, usize)> {
        self.browser_session
            .borrow()
            .as_ref()
            .map(|session| (session.browser_id().to_owned(), session.cookie_count()))
    }

    fn connection_clients(
        settings: &Settings,
        session: Option<yt_media::BrowserCookieSession>,
    ) -> yt_media::Result<(YoutubeClient, Downloader)> {
        let cookies_file = settings
            .extra
            .get("youtube_cookie_file")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from);
        let proxy = settings
            .extra
            .get("youtube_proxy")
            .or_else(|| settings.extra.get("proxy"))
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned);
        let options = yt_media::ConnectionOptions {
            socket_timeout_seconds: settings.download_socket_timeout as u64,
            proxy,
            cookies_file,
        };
        Ok((
            match session {
                Some(session) => YoutubeClient::with_browser_session(options.clone(), session)?,
                None => YoutubeClient::with_options(options.clone())?,
            },
            Downloader::with_options(options)?,
        ))
    }
    #[cfg(test)]
    pub(crate) fn for_test(profile: &Path) -> Self {
        let core = AppCore::open(profile).expect("temporary profile");
        Self {
            library_dir: core.library_dir().to_owned(),
            profile: profile.to_owned(),
            plugins: yt_plugins::PluginHost::open(profile.join("Plugins"))
                .map(|host| host.with_library(core.clone()))
                .map_err(|error| error.to_string()),
            core,
            audio: Err("Audio output disabled for tests".into()),
            youtube: RefCell::new(YoutubeClient::new().expect("YouTube client")),
            downloader: RefCell::new(Downloader::new().expect("download client")),
            connection_error: RefCell::new(None),
            browser_session: RefCell::new(None),
            download_slots: DownloadLimiter::new(3),
            processing_slots: DownloadLimiter::new(1),
        }
    }
    pub fn open() -> Result<Self, String> {
        let base = directories::BaseDirs::new().ok_or("No se encontró la carpeta de usuario")?;
        let profile = profile_path()?;
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
        let settings = core.settings().map_err(|e| e.to_string())?;
        let (youtube, downloader, connection_error) =
            match Self::connection_clients(&settings, None) {
                Ok((youtube, downloader)) => (youtube, downloader, None),
                Err(error) => (
                    YoutubeClient::new().map_err(|e| e.to_string())?,
                    Downloader::new().map_err(|e| e.to_string())?,
                    Some(error.to_string()),
                ),
            };
        Ok(Self {
            plugins: yt_plugins::PluginHost::open(profile.join("Plugins"))
                .map(|host| host.with_library(core.clone()))
                .map_err(|error| error.to_string()),
            core,
            audio: AudioPlayer::new().map_err(|e| e.to_string()),
            youtube: RefCell::new(youtube),
            downloader: RefCell::new(downloader),
            connection_error: RefCell::new(connection_error),
            browser_session: RefCell::new(None),
            profile,
            library_dir,
            download_slots: DownloadLimiter::new(settings.max_concurrent_downloads as usize),
            processing_slots: DownloadLimiter::new(settings.max_concurrent_processing as usize),
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
    pub thumbnail: String,
    pub resumable: bool,
    pub discarded: bool,
}

#[derive(Clone)]
pub struct AppState {
    pub services: Rc<Services>,
    pub page: Signal<Page>,
    pub settings_section: Signal<String>,
    pub plugins_revision: Signal<u64>,
    pub library: Signal<Vec<Track>>,
    pub playlists: Signal<Vec<Playlist>>,
    pub settings: Signal<Settings>,
    pub current: Signal<Option<Track>>,
    pub video_track: Signal<Option<Track>>,
    pub video_open: Signal<bool>,
    pub video_generation: Signal<u64>,
    pub video_url: Signal<Option<String>>,
    pub stream_request: Signal<Option<CancellationToken>>,
    pub stream_session: Signal<Option<yt_media::PlaybackSession>>,
    pub stream_generation: Signal<u64>,
    pub stream_loading: Signal<bool>,
    pub pending_seek: Signal<Option<f64>>,
    pub seek_busy: Signal<bool>,
    pub history: Signal<Vec<Track>>,
    pub queue: Signal<Vec<Track>>,
    pub playing: Signal<bool>,
    pub position: Signal<f64>,
    pub duration: Signal<f64>,
    pub volume: Signal<f32>,
    pub previous_volume: Signal<f32>,
    pub playback_error: Signal<Option<String>>,
    pub buffering: Signal<bool>,
    pub recovering: Signal<bool>,
    pub recovery_attempts: Signal<u8>,
    pub rate: Signal<f32>,
    pub shuffle: Signal<bool>,
    pub repeat: Signal<u8>,
    pub queue_open: Signal<bool>,
    pub options_open: Signal<bool>,
    pub menu_track: Signal<Option<Track>>,
    pub chipmunk_track: Signal<Option<Track>>,
    pub information_track: Signal<Option<Track>>,
    pub notice: Signal<Option<String>>,
    notice_generation: Signal<u64>,
    pub busy: Signal<bool>,
    pub downloads: Signal<Vec<DownloadView>>,
    pub cancellations: Signal<HashMap<String, CancellationToken>>,
    pub sleep_at: Signal<Option<Instant>>,
    last_saved_playback: Rc<RefCell<Option<PlaybackState>>>,
}

fn restore_playback(core: &AppCore, state: &PlaybackState) -> (Option<Track>, Vec<Track>) {
    let find_track = |id: &String| {
        core.track(id)
            .ok()
            .filter(|track| !track.missing)
            .or_else(|| {
                state
                    .remote_tracks
                    .iter()
                    .find(|track| &track.id == id && crate::remote::is_remote(track))
                    .cloned()
            })
    };
    let current_id = state.current_index.and_then(|index| state.queue.get(index));
    let current = current_id
        .and_then(find_track)
        .filter(|track| !track.missing);
    let queue = state
        .queue
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != state.current_index)
        .filter_map(|(_, id)| find_track(id))
        .filter(|track| !track.missing)
        .collect();
    (current, queue)
}

pub(crate) fn download_view(job: &Job) -> DownloadView {
    DownloadView {
        id: job.id.clone(),
        title: job.payload["title"].as_str().unwrap_or("Descarga").into(),
        progress: job.progress,
        status: match job.status {
            JobStatus::Pending => "Preparando",
            JobStatus::Running => "Descargando",
            JobStatus::Completed => "Completado",
            JobStatus::Error => "Error",
            JobStatus::Canceled if job.result["discarded"] == true => "Descartado",
            JobStatus::Canceled if job.result["resumable"] == true => "En pausa",
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
        thumbnail: job.payload["thumbnail"].as_str().unwrap_or_default().into(),
        resumable: job.result["resumable"] == true || job.status == JobStatus::Interrupted,
        discarded: job.result["discarded"] == true,
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

// Application state is shared by tasks which deliberately outlive individual pages.
// Give those signals the same lifetime as the application task scope.
fn use_app_signal<T: 'static>(init: impl FnOnce() -> T) -> Signal<T> {
    use_hook(|| Signal::new_in_scope(init(), ScopeId::ROOT))
}

pub fn use_app_state(services: Rc<Services>) -> AppState {
    let core = &services.core;
    let initial_result = use_hook(|| core.load_playback().map_err(|e| e.to_string()));
    let initial = initial_result.clone().unwrap_or_default();
    let restored = use_hook(|| restore_playback(core, &initial));
    let library = use_app_signal(|| core.library("").unwrap_or_default());
    let playlists = use_app_signal(|| core.playlists().unwrap_or_default());
    let settings = use_app_signal(|| core.settings().unwrap_or_default());
    let history = use_app_signal(|| {
        let mut seen = std::collections::HashSet::new();
        core.history(50)
            .unwrap_or_default()
            .into_iter()
            .map(|h| h.track)
            .filter(|track| seen.insert(track.id.clone()) && !track.missing)
            .collect()
    });
    let queue = use_app_signal(|| restored.1.clone());
    AppState {
        services: services.clone(),
        page: use_app_signal(|| Page::Home),
        settings_section: use_app_signal(String::new),
        plugins_revision: use_app_signal(|| 0),
        library,
        playlists,
        settings,
        current: use_app_signal(|| restored.0.clone()),
        video_track: use_app_signal(|| None),
        video_open: use_app_signal(|| false),
        video_generation: use_app_signal(|| 0),
        video_url: use_app_signal(|| None),
        stream_request: use_app_signal(|| None),
        stream_session: use_app_signal(|| None),
        stream_generation: use_app_signal(|| 0),
        stream_loading: use_app_signal(|| false),
        pending_seek: use_app_signal(|| None),
        seek_busy: use_app_signal(|| false),
        history,
        queue,
        playing: use_app_signal(|| false),
        position: use_app_signal(|| {
            if restored.0.is_some() {
                initial.position_seconds
            } else {
                0.0
            }
        }),
        duration: use_app_signal(|| restored.0.as_ref().map(|t| t.duration).unwrap_or(0.0)),
        previous_volume: use_app_signal(|| {
            if initial.volume > 0.0 {
                initial.volume as f32
            } else {
                0.7
            }
        }),
        playback_error: use_app_signal(|| None),
        buffering: use_app_signal(|| false),
        recovering: use_app_signal(|| false),
        recovery_attempts: use_app_signal(|| 0),
        volume: use_app_signal(|| {
            if initial.playback_rate > 0.0 {
                initial.volume as f32
            } else {
                0.8
            }
        }),
        rate: use_app_signal(|| {
            if initial.playback_rate > 0.0 {
                initial.playback_rate as f32
            } else {
                1.0
            }
        }),
        shuffle: use_app_signal(|| initial.shuffle),
        repeat: use_app_signal(|| match initial.repeat.as_str() {
            "all" => 1,
            "one" => 2,
            _ => 0,
        }),
        queue_open: use_app_signal(|| false),
        options_open: use_app_signal(|| false),
        menu_track: use_app_signal(|| None),
        chipmunk_track: use_app_signal(|| None),
        information_track: use_app_signal(|| None),
        notice: use_app_signal(|| {
            initial_result
                .err()
                .map(|e| format!("No se pudo restaurar la reproducción: {e}"))
        }),
        notice_generation: use_app_signal(|| 0),
        busy: use_app_signal(|| false),
        downloads: use_app_signal(|| {
            core.jobs()
                .unwrap_or_default()
                .iter()
                .filter(|job| matches!(job.kind.as_str(), "download" | "playlist_download"))
                .map(download_view)
                .collect()
        }),
        cancellations: use_app_signal(HashMap::new),
        sleep_at: use_app_signal(|| None),
        last_saved_playback: use_hook(|| Rc::new(RefCell::new(None))),
    }
}

impl AppState {
    pub fn notify(&self, text: impl Into<String>) {
        let mut notice = self.notice;
        let text = text.into();
        crate::native::record_log(&self.services.profile, &text);
        notice.set(Some(text));
        let mut sequence = self.notice_generation;
        let generation = sequence.peek().wrapping_add(1);
        sequence.set(generation);
        dioxus::core::spawn_forever(async move {
            tokio::time::sleep(Duration::from_secs(4)).await;
            if *sequence.peek() == generation {
                notice.set(None);
            }
        });
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
        let mut error = self.playback_error;
        error.set(None);
        let mut recovery = self.recovering;
        recovery.set(false);
        if crate::remote::is_remote(&track) {
            self.start_remote(track, 0.0);
            return;
        }
        self.cancel_remote();
        let mut video_url = self.video_url;
        video_url.set(None);
        if track.media_type == "video" {
            if let Ok(audio) = &self.services.audio {
                if let Err(error) = audio.stop() {
                    self.notify(error.to_string());
                    return;
                }
            }
            let mut state = self.clone();
            state.playing.set(true);
            state.position.set(0.0);
            state.duration.set(track.duration);
            state.current.set(Some(track.clone()));
            state.video_track.set(Some(track.clone()));
            state.video_open.set(true);
            state.video_generation += 1;
            state
                .history
                .write()
                .retain(|previous| previous.id != track.id);
            state.history.write().insert(0, track.clone());
            if let Err(error) = state.services.core.record_playback(&track.id, 0.0) {
                state.notify(error.to_string());
            }
            crate::native::record_recent(&track.path);
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
                s.playback_error.set(None);
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
                crate::native::record_recent(&track.path);
                s.persist_playback();
            }
            Err(error) => {
                let mut playback_error = self.playback_error;
                playback_error.set(Some(error.clone()));
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
        if *self.stream_loading.peek() {
            self.cancel_remote();
            return;
        }
        if self.playback_error.peek().is_some() {
            self.retry_playback();
            return;
        }
        if crate::remote::is_remote(&track) {
            let unloaded = if track.media_type == "video" {
                self.video_track.peek().is_none()
            } else {
                self.services
                    .audio
                    .as_ref()
                    .is_ok_and(|audio| audio.snapshot().remote_id.is_none())
            };
            if unloaded {
                let position = *self.position.peek();
                self.start_remote(track, position);
                return;
            }
        }
        if track.media_type == "video" {
            if self.video_track.peek().is_none() {
                let mut state = self.clone();
                state.video_track.set(Some(track));
                state.video_open.set(true);
                state.video_generation += 1;
                state.playing.set(true);
            } else {
                let operation = if *self.playing.peek() {
                    "pause"
                } else {
                    "play"
                };
                crate::video::command(self.clone(), serde_json::json!({"op": operation}));
            }
            return;
        }
        let Ok(audio) = &self.services.audio else {
            self.play(track);
            return;
        };
        let mut s = self.clone();
        let was_playing = *s.playing.read();
        if audio.snapshot().path.is_none() && audio.snapshot().remote_id.is_none() {
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
            if *self.stream_loading.peek() {
                self.cancel_remote();
            }
            if self.video_track.peek().is_some() {
                crate::video::command(self.clone(), serde_json::json!({"op":"pause"}));
            }
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
        if position > 3.0 {
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
        if is_video {
            let mut position = self.position;
            position.set(seconds);
            crate::video::command(
                self.clone(),
                serde_json::json!({"op":"seek","value":seconds}),
            );
            self.persist_playback();
            return;
        }
        if let Ok(audio) = &self.services.audio {
            if audio.snapshot().remote_id.is_some() {
                self.seek_remote(seconds);
                return;
            }
            if audio.snapshot().path.is_none() && audio.snapshot().remote_id.is_none() {
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
        if value > 0.0 {
            let mut previous = self.previous_volume;
            previous.set(value);
        }
        let mut volume = self.volume;
        volume.set(value);
        if self.video_track.peek().is_some() {
            crate::video::command(
                self.clone(),
                serde_json::json!({"op":"volume","value":value}),
            );
            self.persist_playback();
            return;
        }
        if let Ok(audio) = &self.services.audio {
            if let Err(e) = audio.set_volume(value) {
                self.notify(e.to_string());
            }
        }
        self.persist_playback();
    }

    pub fn toggle_mute(&self) {
        let value = if *self.volume.peek() > 0.0 {
            0.0
        } else {
            *self.previous_volume.peek()
        };
        self.set_volume(value);
    }
    pub fn retry_playback(&self) {
        let current = self.current.peek().clone();
        let position = *self.position.peek();
        if let Some(track) = current {
            if crate::remote::is_remote(&track) {
                self.start_remote(track, position);
            } else if track.media_type == "video" {
                self.play(track);
                self.seek(position);
            } else {
                self.play_at(track, position, false);
            }
        }
    }

    pub fn set_rate(&self, value: f32) {
        if !value.is_finite() || !(0.25..=4.0).contains(&value) {
            self.notify("La velocidad debe estar entre 0,25× y 4×");
            return;
        }
        if self.video_track.peek().is_some() {
            let mut rate = self.rate;
            rate.set(value);
            crate::video::command(self.clone(), serde_json::json!({"op":"rate","value":value}));
            self.persist_playback();
            return;
        }
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
            remote_tracks: self
                .current
                .peek()
                .iter()
                .chain(self.queue.peek().iter())
                .filter(|track| crate::remote::is_remote(track))
                .cloned()
                .collect(),
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
        let connection_changed = {
            let previous = self.settings.peek();
            value.download_socket_timeout != previous.download_socket_timeout
                || value.extra.get("youtube_cookie_file")
                    != previous.extra.get("youtube_cookie_file")
                || value.extra.get("proxy") != previous.extra.get("proxy")
                || value.extra.get("youtube_proxy") != previous.extra.get("youtube_proxy")
                || self.services.connection_error.borrow().is_some()
        };
        let clients = if connection_changed {
            match Services::connection_clients(
                &value,
                self.services.browser_session.borrow().clone(),
            ) {
                Ok(clients) => Some(clients),
                Err(error) => {
                    self.notify(format!("No se guardó la conexión: {error}"));
                    return;
                }
            }
        } else {
            None
        };
        match self.services.core.save_settings(&value) {
            Ok(()) => {
                if let Some((youtube, downloader)) = clients {
                    *self.services.youtube.borrow_mut() = youtube;
                    *self.services.downloader.borrow_mut() = downloader;
                    *self.services.connection_error.borrow_mut() = None;
                }
                self.services
                    .download_slots
                    .set_limit(value.max_concurrent_downloads as usize);
                self.services
                    .processing_slots
                    .set_limit(value.max_concurrent_processing as usize);
                let mut settings = self.settings;
                settings.set(value);
            }
            Err(e) => self.notify(format!("No se guardaron los ajustes: {e}")),
        }
    }

    pub fn update_setting(&self, key: &str, value: serde_json::Value) {
        let result = serde_json::to_value(self.settings.peek().clone()).and_then(|mut settings| {
            settings[key] = value;
            serde_json::from_value::<Settings>(settings)
        });
        match result {
            Ok(settings) => self.save_settings(settings),
            Err(error) => self.notify(format!("No se pudo guardar el ajuste: {error}")),
        }
    }

    pub fn plugin_event(&self, name: &str, payload: serde_json::Value) {
        if !self.settings.peek().developer_mode {
            return;
        }
        let Ok(host) = self.services.plugins.clone() else {
            return;
        };
        let event = yt_plugins::PluginEvent {
            name: name.to_owned(),
            payload,
        };
        let state = self.clone();
        dioxus::core::spawn_forever(async move {
            match tokio::task::spawn_blocking(move || host.dispatch_event(event)).await {
                Ok(Ok(deliveries)) => {
                    if deliveries.iter().any(|delivery| delivery.library_changed) {
                        state.refresh();
                    }
                }
                Ok(Err(error)) => state.notify(format!(
                    "No se pudo enviar el evento a los plugins: {error}"
                )),
                Err(error) => state.notify(format!("La tarea del plugin se interrumpió: {error}")),
            }
        });
    }

    pub fn clean_temporary_files(&self) {
        if !self.cancellations.peek().is_empty() {
            self.notify("Espera a que terminen los trabajos antes de limpiar temporales");
            return;
        }
        let state = self.clone();
        dioxus::core::spawn_forever(async move {
            let core = state.services.core.clone();
            match tokio::task::spawn_blocking(move || core.clean_temporary_files()).await {
                Ok(Ok(report)) => state.notify(format!(
                    "{} temporales eliminados · {} liberados{}",
                    report.files_removed,
                    size_label(report.bytes_removed),
                    if report.errors.is_empty() {
                        String::new()
                    } else {
                        format!("; {} archivos no se pudieron limpiar", report.errors.len())
                    }
                )),
                Ok(Err(error)) => {
                    state.notify(format!("No se pudieron limpiar los temporales: {error}"))
                }
                Err(error) => {
                    state.notify(format!("No se pudieron limpiar los temporales: {error}"))
                }
            }
        });
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

    pub fn export_playlist(&self, playlist: Playlist) {
        let state = self.clone();
        dioxus::core::spawn_forever(async move {
            let Some(output) = rfd::AsyncFileDialog::new()
                .set_title("Exportar playlist")
                .set_file_name(yt_media::safe_file_name(&playlist.name, "m3u8"))
                .add_filter("Playlist M3U8", &["m3u8"])
                .save_file()
                .await
            else {
                return;
            };
            let core = state.services.core.clone();
            let path = output.path().to_owned();
            match tokio::task::spawn_blocking(move || {
                core.export_playlist_m3u8(&playlist.id, &path)
            })
            .await
            {
                Ok(Ok(report)) => state.notify(format!(
                    "Playlist exportada: {} pistas{}",
                    report.exported_tracks,
                    if report.skipped.is_empty() {
                        String::new()
                    } else {
                        format!("; {} archivos no disponibles", report.skipped.len())
                    }
                )),
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
                for track in &report.tracks {
                    crate::native::record_recent(&track.path);
                }
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

    pub fn start_polling(&self) {
        self.plugin_event(
            "app.started",
            serde_json::json!({"version":env!("CARGO_PKG_VERSION")}),
        );
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
                        track.media_type != "video"
                            && (snapshot.path.as_ref() == Some(&track.path)
                                || snapshot
                                    .remote_id
                                    .as_ref()
                                    .is_some_and(|id| track.id == format!("youtube:{id}")))
                    });
                    if current_is_audio {
                        s.buffering.set(
                            snapshot
                                .streaming
                                .as_ref()
                                .is_some_and(|stream| stream.buffering),
                        );
                        let old_position = *s.position.peek();
                        let old_duration = *s.duration.peek();
                        let was_playing = *s.playing.peek();
                        if old_position != snapshot.position_seconds && !*s.seek_busy.peek() {
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
                            s.recover_remote(error.clone());
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
        // Resolving/opening a remote source has no player to pause yet. Cancel
        // that request so its completion cannot restart playback after expiry.
        if *self.stream_loading.peek() {
            self.cancel_remote();
        }
        let pause_error = self
            .services
            .audio
            .as_ref()
            .ok()
            .and_then(|audio| audio.pause().err());
        let mut state = self.clone();
        if state.video_track.peek().is_some() {
            crate::video::command(state.clone(), serde_json::json!({"op":"pause"}));
        }
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
    fn restored_remote_queue_rejects_wrong_identity_and_never_requires_local_media() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let item = yt_media::SearchResult {
            id: "aqz-KE-bpKQ".into(),
            url: "https://www.youtube.com/watch?v=aqz-KE-bpKQ".into(),
            title: "Remote queue".into(),
            thumbnail: String::new(),
            duration_seconds: 635,
            channel: "Channel".into(),
            view_count: 0,
        };
        let remote = crate::remote::track_from_result(item, false);
        let mut invalid = remote.clone();
        invalid.id = "youtube:invalid".into();
        let playback = PlaybackState {
            queue: vec![remote.id.clone(), invalid.id.clone()],
            current_index: Some(0),
            remote_tracks: vec![remote.clone(), invalid],
            position_seconds: 41.5,
            ..Default::default()
        };
        let (current, queue) = restore_playback(&core, &playback);
        assert_eq!(current.unwrap().id, remote.id);
        assert!(queue.is_empty());
        assert!(core.library("").unwrap().is_empty());
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

    #[tokio::test]
    async fn recovery_is_bounded_preserves_position_and_cancels_when_paused_or_replaced() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let local = services.core.import_bytes("local.mp4", &[1, 2, 3]).unwrap();
        let capture: CapturedState = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(action_harness);
        dom.insert_any_root_context(Box::new(services));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let mut state = capture.borrow().as_ref().unwrap().clone();
        let remote = crate::remote::track_from_result(
            yt_media::SearchResult {
                id: "aqz-KE-bpKQ".into(),
                url: "https://www.youtube.com/watch?v=aqz-KE-bpKQ".into(),
                title: "Remote".into(),
                duration_seconds: 635,
                channel: String::new(),
                thumbnail: String::new(),
                view_count: 0,
            },
            true,
        );
        dom.in_scope(ScopeId::ROOT, || {
            state.current.set(Some(remote.clone()));
            state.position.set(32.5);
            state.playing.set(true);
            state.recover_remote("Connection reset".into());
            assert!(*state.recovering.peek());
            assert!(*state.stream_loading.peek());
            assert!(!*state.playing.peek());
            assert_eq!(*state.position.peek(), 32.5);
            let pending = state.stream_request.peek().clone().unwrap();
            let generation = *state.stream_generation.peek();
            state.recover_remote("Repeated event".into());
            assert_eq!(*state.stream_generation.peek(), generation);
            state.toggle_play();
            assert!(pending.is_cancelled());
            assert!(!*state.recovering.peek());
            assert!(state.stream_request.peek().is_none());
            state.recover_remote("Second reset".into());
            let pending = state.stream_request.peek().clone().unwrap();
            state.play(local.clone());
            assert!(pending.is_cancelled());
            assert_eq!(state.current.peek().as_ref().unwrap().id, local.id);
            state.recover_remote("Unsupported local video".into());
            assert!(!*state.playing.peek());
            assert!(state.playback_error.peek().is_some());
            assert!(state.stream_request.peek().is_none());
            state.current.set(Some(remote));
            state.recovery_attempts.set(2);
            state.playback_error.set(None);
            state.recover_remote("Retries exhausted".into());
            assert_eq!(
                state.playback_error.peek().as_deref(),
                Some("Retries exhausted")
            );
            assert!(!*state.recovering.peek());
            assert!(state.stream_request.peek().is_none());
            state.set_volume(0.35);
            state.toggle_mute();
            assert_eq!(*state.volume.peek(), 0.0);
            state.toggle_mute();
            assert_eq!(*state.volume.peek(), 0.35);
        });
    }

    fn action_harness() -> Element {
        let services = use_context::<Rc<Services>>();
        let capture = use_context::<CapturedState>();
        let state = use_app_state(services);
        *capture.borrow_mut() = Some(state);
        rsx! { div { "Action test" } }
    }

    #[tokio::test]
    async fn restored_remote_video_resumes_without_borrowing_position_during_start() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let remote = crate::remote::track_from_result(
            yt_media::SearchResult {
                id: "aqz-KE-bpKQ".into(),
                url: "https://www.youtube.com/watch?v=aqz-KE-bpKQ".into(),
                title: "Restored video".into(),
                thumbnail: String::new(),
                duration_seconds: 635,
                channel: String::new(),
                view_count: 0,
            },
            true,
        );
        services
            .core
            .save_playback(&PlaybackState {
                queue: vec![remote.id.clone()],
                remote_tracks: vec![remote.clone()],
                current_index: Some(0),
                position_seconds: 41.5,
                ..Default::default()
            })
            .unwrap();
        let capture: CapturedState = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(action_harness);
        dom.insert_any_root_context(Box::new(services));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let state = capture.borrow().as_ref().unwrap().clone();
        dom.in_scope(ScopeId::ROOT, || {
            state.toggle_play();
            assert!(*state.stream_loading.peek());
            assert_eq!(*state.position.peek(), 41.5);
            assert_eq!(state.current.peek().as_ref().unwrap().id, remote.id);
            // Cancel before polling tasks; this test never contacts YouTube.
            state.cancel_remote();
        });
    }

    #[tokio::test]
    async fn sleep_expiry_invalidates_pending_remote_but_keeps_loaded_stream_resumable() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let capture: CapturedState = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(action_harness);
        dom.insert_any_root_context(Box::new(services));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let mut state = capture.borrow().as_ref().unwrap().clone();
        dom.in_scope(ScopeId::ROOT, || {
            let pending = CancellationToken::new();
            state.stream_request.set(Some(pending.clone()));
            state.stream_loading.set(true);
            state.pending_seek.set(Some(30.0));
            state.sleep_at.set(Some(Instant::now()));
            let generation = *state.stream_generation.peek();
            state.expire_sleep_timer();
            assert!(pending.is_cancelled());
            assert_eq!(*state.stream_generation.peek(), generation + 1);
            assert!(!*state.stream_loading.peek());
            assert!(state.pending_seek.peek().is_none());
            assert!(state.stream_request.peek().is_none());
            assert!(state.sleep_at.peek().is_none());
            assert!(!*state.playing.peek());

            let loaded = CancellationToken::new();
            state.stream_request.set(Some(loaded.clone()));
            state.playing.set(true);
            state.sleep_at.set(Some(Instant::now()));
            state.expire_sleep_timer();
            assert!(!loaded.is_cancelled());
            assert!(!*state.playing.peek());
            assert_eq!(*state.stream_generation.peek(), generation + 1);
        });
    }

    #[tokio::test]
    async fn next_without_queue_cancels_a_pending_remote_instead_of_starting_it_later() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let capture: CapturedState = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(action_harness);
        dom.insert_any_root_context(Box::new(services));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let mut state = capture.borrow().as_ref().unwrap().clone();
        dom.in_scope(ScopeId::ROOT, || {
            let token = CancellationToken::new();
            state.stream_request.set(Some(token.clone()));
            state.stream_loading.set(true);
            state.next(false);
            assert!(token.is_cancelled());
            assert!(!*state.stream_loading.peek());
            assert!(!*state.playing.peek());
            assert_eq!(*state.stream_generation.peek(), 1);
        });
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
            assert_eq!(state.current.peek().as_ref().unwrap().id, second.id);
            assert_eq!(*state.position.peek(), 0.0);
            state.previous();
            assert_eq!(state.current.peek().as_ref().unwrap().id, third.id);
            state.video_track.set(None);
            state.toggle_play();
            assert_eq!(state.video_track.peek().as_ref().unwrap().id, third.id);
            state.seek(30.0);
            assert_eq!(*state.position.peek(), 30.0);
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
            assert!(state.video_track.peek().is_some());
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
