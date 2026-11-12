use dioxus::prelude::*;
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
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
use yt_media::{AudioPlayer, CancellationToken, Downloader, Transition, YoutubeClient};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Home,
    NewReleases,
    Radio,
    Search,
    Recent,
    Artists,
    Albums,
    Library,
    Favorites,
    Playlists,
    Stems,
    Downloads,
    Settings,
}

impl Page {
    /// Every page, in sidebar order.
    pub const ALL: [Self; 13] = [
        Self::Home,
        Self::NewReleases,
        Self::Radio,
        Self::Search,
        Self::Recent,
        Self::Artists,
        Self::Albums,
        Self::Library,
        Self::Favorites,
        Self::Playlists,
        Self::Stems,
        Self::Downloads,
        Self::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "Inicio",
            Self::NewReleases => "Novedades",
            Self::Radio => "Radio",
            Self::Search => "Buscar",
            Self::Recent => "Recientes",
            Self::Artists => "Artistas",
            Self::Albums => "Álbumes",
            Self::Library => "Canciones",
            Self::Favorites => "Favoritos",
            Self::Playlists => "Playlists",
            Self::Stems => "Stems",
            Self::Downloads => "Descargas",
            Self::Settings => "Ajustes",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::NewReleases => "sparkles",
            Self::Radio => "radio",
            Self::Search => "search",
            Self::Recent => "clock",
            Self::Artists => "mic",
            Self::Albums => "album",
            Self::Library => "music-2",
            Self::Favorites => "heart",
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

    /// Connection preferences of network clients: the socket timeout, the
    /// proxy and the cookie file the user chose for YouTube.
    pub(crate) fn connection_options(settings: &Settings) -> yt_media::ConnectionOptions {
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
        yt_media::ConnectionOptions {
            socket_timeout_seconds: settings.download_socket_timeout as u64,
            proxy,
            cookies_file,
        }
    }

    fn connection_clients(
        settings: &Settings,
        session: Option<yt_media::BrowserCookieSession>,
    ) -> yt_media::Result<(YoutubeClient, Downloader)> {
        let options = Self::connection_options(settings);
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
    pub session_history: Signal<Vec<Track>>,
    suppress_session_history: Rc<Cell<bool>>,
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
    /// Song whose credits dialog is open.
    pub credits_track: Signal<Option<Track>>,
    pub activity: crate::activity::Activity,
    pub busy: Signal<bool>,
    pub downloads: Signal<Vec<DownloadView>>,
    pub cancellations: Signal<HashMap<String, CancellationToken>>,
    pub sleep_at: Signal<Option<Instant>>,
    pub(crate) listening: crate::listening::Listening,
    /// Ids of every favorite, for marking items anywhere without a query.
    pub favorites: Signal<HashSet<String>>,
    /// Bumped after every favorite change so the Favorites page reloads.
    pub favorites_revision: Signal<u64>,
    pub(crate) autoplay: crate::autoplay::Autoplay,
    pub(crate) dj: crate::dj::Dj,
    pub(crate) dj_analysis: crate::dj_analysis::DjAnalysis,
    /// The blend into the playing song, while the engine plays one.
    pub(crate) blend: Signal<Option<yt_media::Blend>>,
    /// The mix planned out of the playing song, for the player.
    pub(crate) next_mix: Signal<Option<crate::dj::NextMix>>,
    /// What the mix being heard is, from when it was armed until its blend ends.
    pub(crate) mixing_as: Signal<Option<crate::dj::MixName>>,
    /// The song that leaves in the blend being heard and how its music is
    /// timed, from when the blend was started until it ends.
    pub(crate) blend_scene: Signal<Option<crate::dj::BlendScene>>,
    pub(crate) discovery: crate::discovery::Discovery,
    pub(crate) album_art: crate::album_art::AlbumArt,
    /// The full-screen now-playing view is open.
    pub now_playing_open: Signal<bool>,
    pub(crate) lyrics: crate::lyrics::LyricsState,
    /// Detection of the zone used for nearby concerts.
    pub(crate) region: crate::region::Region,
    /// Detail views above the current page, top last.
    pub(crate) detail: Signal<Vec<crate::navigation::Detail>>,
    /// Playlist shown by the Playlists page.
    pub selected_playlist: Signal<Option<String>>,
    /// Followed artists, most recent first.
    pub followed_artists: Signal<Vec<yt_core::FollowedArtist>>,
    /// Remembered searches, most recent first.
    pub search_history: Signal<Vec<yt_core::SearchHistoryEntry>>,
    /// Artwork palettes shared by the playing theme and colored surfaces.
    pub(crate) palettes: crate::theme::Palettes,
    last_saved_playback: Rc<RefCell<Option<PlaybackState>>>,
}

// Remote audio/video share their service identity, but their persisted queue
// references must retain the chosen mode. Older bare identities remain readable.
fn playback_key(track: &Track) -> String {
    if crate::remote::is_remote(track) {
        format!(
            "{}:{}",
            track.id,
            if track.media_type == "video" {
                "video"
            } else {
                "audio"
            }
        )
    } else {
        track.id.clone()
    }
}

fn restore_playback(
    core: &AppCore,
    state: &PlaybackState,
) -> (Option<Track>, Vec<Track>, Vec<Track>) {
    let find_track = |id: &String| {
        core.track(id)
            .ok()
            .filter(|track| !track.missing)
            .or_else(|| {
                state
                    .remote_tracks
                    .iter()
                    .find(|track| {
                        crate::remote::is_remote(track)
                            && (&track.id == id || playback_key(track) == *id)
                    })
                    .map(|track| Track {
                        artist: yt_core::clean_artist(&track.artist),
                        ..track.clone()
                    })
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
    let history = state
        .session_history
        .iter()
        .rev()
        .take(100)
        .filter_map(find_track)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    (current, queue, history)
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
/// Recently played items kept in memory for Home and the queue panel.
const RECENT_HISTORY_LIMIT: usize = 50;

pub(crate) fn use_app_signal<T: 'static>(init: impl FnOnce() -> T) -> Signal<T> {
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
        core.recently_played(RECENT_HISTORY_LIMIT)
            .unwrap_or_default()
            .iter()
            .map(crate::remote::track_from_item)
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
        session_history: use_app_signal(|| restored.2.clone()),
        suppress_session_history: use_hook(|| Rc::new(Cell::new(false))),
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
        credits_track: use_app_signal(|| None),
        activity: crate::activity::Activity::use_new(
            initial_result
                .err()
                .map(|e| format!("No se pudo restaurar la reproducción: {e}")),
        ),
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
        listening: use_hook(crate::listening::Listening::default),
        favorites: use_app_signal(|| core.favorite_ids().unwrap_or_default()),
        favorites_revision: use_app_signal(|| 0),
        autoplay: crate::autoplay::use_autoplay(),
        dj: use_hook(crate::dj::Dj::default),
        dj_analysis: crate::dj_analysis::use_dj_analysis(),
        blend: use_app_signal(|| None),
        next_mix: use_app_signal(|| None),
        mixing_as: use_app_signal(|| None),
        blend_scene: use_app_signal(|| None),
        discovery: crate::discovery::use_discovery(),
        album_art: crate::album_art::use_album_art(core, settings),
        now_playing_open: use_app_signal(|| false),
        lyrics: crate::lyrics::use_lyrics(),
        region: crate::region::use_region(),
        detail: use_app_signal(Vec::new),
        selected_playlist: use_app_signal(|| None),
        followed_artists: use_app_signal(|| core.followed_artists().unwrap_or_default()),
        search_history: use_app_signal(|| {
            core.search_history(crate::search::HISTORY_IN_MEMORY)
                .unwrap_or_default()
        }),
        palettes: use_hook(crate::theme::Palettes::default),
        last_saved_playback: use_hook(|| Rc::new(RefCell::new(None))),
    }
}

impl AppState {
    pub fn refresh(&self) {
        let mut library = self.library;
        let mut playlists = self.playlists;
        match self.services.core.library("") {
            Ok(v) => library.set(v),
            Err(e) => self.notify_error(e.to_string()),
        }
        match self.services.core.playlists() {
            Ok(v) => playlists.set(v),
            Err(e) => self.notify_error(e.to_string()),
        }
    }

    /// Play `track` because the user picked it: this starts a new listening
    /// context, so a playing station ends.
    pub fn play(&self, track: Track) {
        self.stop_station();
        self.play_with(track, Transition::Cut);
    }

    /// Play `track` from the start, replacing what plays with `transition`.
    pub(crate) fn play_with(&self, track: Track, transition: Transition) {
        let mut error = self.playback_error;
        error.set(None);
        let mut recovery = self.recovering;
        recovery.set(false);
        if crate::remote::is_remote(&track) {
            self.start_remote_with(track, 0.0, transition);
            return;
        }
        if track.media_type == "video" {
            crate::video::start_local(self.clone(), track, 0.0);
            return;
        }
        // A blending online song keeps its session: the engine releases it
        // when the fade ends.
        self.reset_remote(transition == Transition::Cut);
        let mut video_url = self.video_url;
        video_url.set(None);
        let mut video = self.video_track;
        video.set(None);
        self.play_at(track, 0.0, true, transition);
    }

    /// Every newly started playback passes here once: it opens the listening
    /// event and lets autoplay prepare what follows.
    pub(crate) fn playback_started(&self, track: &Track) {
        self.reset_blend();
        self.begin_listening(track);
        self.refresh_autoplay();
    }

    fn play_at(&self, track: Track, position: f64, record: bool, transition: Transition) {
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
                audio
                    .play_with(&track.path, transition)
                    .map_err(|e| e.to_string())?;
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
                s.record_session_departure(&track);
                s.current.set(Some(track.clone()));
                s.history.write().retain(|t| t.id != track.id);
                s.history.write().insert(0, track.clone());
                if record {
                    s.playback_started(&track);
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
                self.notify_error(format!("No se pudo reproducir: {error}"));
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
                let position = *self.position.peek();
                crate::video::start_local(self.clone(), track, position);
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
            // Nothing is loaded after a restart: resuming starts a new listen.
            let position = *self.position.peek();
            self.play_at(track, position, true, Transition::Cut);
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
            Err(e) => s.notify_error(e.to_string()),
        }
    }

    pub fn play_all(&self, mut tracks: Vec<Track>, shuffled: bool) {
        self.stop_station();
        if shuffled {
            use rand::seq::SliceRandom;
            tracks.shuffle(&mut rand::rng());
        }
        if tracks.is_empty() {
            return;
        }
        let first = tracks.remove(0);
        let mut history = self.session_history;
        history.write().clear();
        let mut queue = self.queue;
        queue.set(tracks);
        self.play_without_session_history(first);
    }

    /// Record only a genuine transition, not a retry, restart, or backward
    /// navigation. Remote playback calls this immediately before changing current.
    pub(crate) fn record_session_departure(&self, next: &Track) {
        if self.suppress_session_history.get() {
            return;
        }
        let previous = self.current.peek().clone();
        if let Some(previous) = previous.filter(|track| playback_key(track) != playback_key(next)) {
            let mut history = self.session_history;
            let mut items = history.write();
            items.push(previous);
            if items.len() > 100 {
                let excess = items.len() - 100;
                items.drain(..excess);
            }
        }
    }

    fn play_without_session_history(&self, track: Track) {
        self.suppress_session_history.set(true);
        self.play_with(track, Transition::Cut);
        self.suppress_session_history.set(false);
    }

    pub fn next(&self, automatic: bool) {
        self.end_listening(automatic);
        let repeat = *self.repeat.peek();
        if automatic && repeat == 2 {
            let current = self.current.peek().clone();
            if let Some(track) = current {
                self.play_without_session_history(track);
            }
            return;
        }
        let mut tracks = self.queue.read().clone();
        if automatic && repeat == 1 && tracks.is_empty() {
            tracks = self.session_history.peek().clone();
            let current = self.current.peek().clone();
            if let Some(current) = current {
                tracks.push(current);
            }
            if !tracks.is_empty() {
                let first = tracks.remove(0);
                let mut history = self.session_history;
                history.write().clear();
                let mut queue = self.queue;
                queue.set(tracks);
                self.play_without_session_history(first);
                return;
            }
        }
        if tracks.is_empty() {
            if let Some(track) = self.take_autoplay() {
                let transition = self.transition_for(&track, automatic);
                self.play_with(track, transition);
                return;
            }
            if *self.stream_loading.peek() {
                self.cancel_remote();
            }
            if self.video_track.peek().is_some() {
                crate::video::command(self.clone(), serde_json::json!({"op":"pause"}));
            }
            if let Ok(audio) = &self.services.audio {
                if let Err(error) = audio.pause() {
                    self.notify_error(error.to_string());
                }
            }
            let mut playing = self.playing;
            playing.set(false);
            self.persist_playback();
            self.autoplay_when_ready();
            return;
        }
        let reserved = self.take_reserved_next();
        let index = if *self.shuffle.read() && !reserved {
            rand::random_range(0..tracks.len())
        } else {
            0
        };
        let track = tracks.remove(index);
        let mut queue = self.queue;
        queue.set(tracks);
        let transition = self.transition_for(&track, automatic);
        self.play_with(track, transition);
    }

    pub fn previous(&self) {
        let position = *self.position.peek();
        let current = self.current.peek().clone();
        if position > 3.0 {
            self.seek(0.0);
            return;
        }
        let mut history = self.session_history;
        let previous = history.write().pop();
        if let Some(track) = previous {
            let original_queue = self.queue.peek().clone();
            if let Some(current) = current {
                let mut queue = self.queue;
                queue.write().insert(0, current);
            }
            self.play_without_session_history(track.clone());
            if self
                .current
                .peek()
                .as_ref()
                .is_none_or(|current| current.id != track.id)
            {
                history.write().push(track);
                let mut queue = self.queue;
                queue.set(original_queue);
            }
            self.persist_playback();
        } else {
            self.seek(0.0);
        }
    }

    pub fn seek(&self, seconds: f64) {
        let is_video = self
            .current
            .peek()
            .as_ref()
            .is_some_and(|track| track.media_type == "video");
        if !seconds.is_finite() {
            self.notify_warning("La posición de reproducción no es válida");
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
                Err(e) => self.notify_error(e.to_string()),
            }
        }
    }

    pub fn set_volume(&self, value: f32) {
        if !value.is_finite() {
            self.notify_warning("El volumen no es válido");
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
                self.notify_error(e.to_string());
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
                crate::video::start_local(self.clone(), track, position);
            } else {
                self.play_at(track, position, false, Transition::Cut);
            }
        }
    }

    pub fn set_rate(&self, value: f32) {
        if !value.is_finite() || !(0.25..=4.0).contains(&value) {
            self.notify_warning("La velocidad debe estar entre 0,25× y 4×");
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
                Err(e) => self.notify_error(e.to_string()),
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
        let mut queue: Vec<String> = self.queue.peek().iter().map(playback_key).collect();
        let current_index = self.current.peek().as_ref().map(|track| {
            queue.insert(0, playback_key(track));
            0
        });
        let state = PlaybackState {
            remote_tracks: self
                .current
                .peek()
                .iter()
                .chain(self.queue.peek().iter())
                .chain(self.session_history.peek().iter())
                .filter(|track| crate::remote::is_remote(track))
                .cloned()
                .collect(),
            session_history: self
                .session_history
                .peek()
                .iter()
                .map(playback_key)
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
            Err(e) => self.notify_error(format!("No se pudo guardar la reproducción: {e}")),
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
                    self.notify_error(format!("No se guardó la conexión: {error}"));
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
            Err(e) => self.notify_error(format!("No se guardaron los ajustes: {e}")),
        }
    }

    /// Change typed preferences; `save_settings` validates them in `yt-core`.
    pub(crate) fn edit_settings(&self, edit: impl FnOnce(&mut Settings)) {
        let mut value = self.settings.peek().clone();
        edit(&mut value);
        self.save_settings(value);
    }

    pub fn update_setting(&self, key: &str, value: serde_json::Value) {
        let result = serde_json::to_value(self.settings.peek().clone()).and_then(|mut settings| {
            settings[key] = value;
            serde_json::from_value::<Settings>(settings)
        });
        match result {
            Ok(settings) => self.save_settings(settings),
            Err(error) => self.notify_error(format!("No se pudo guardar el ajuste: {error}")),
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
                Ok(Err(error)) => state.notify_error(format!(
                    "No se pudo enviar el evento a los plugins: {error}"
                )),
                Err(error) => {
                    state.notify_error(format!("La tarea del plugin se interrumpió: {error}"))
                }
            }
        });
    }

    pub fn clean_temporary_files(&self) {
        if !self.cancellations.peek().is_empty() {
            self.notify_warning("Espera a que terminen los trabajos antes de limpiar temporales");
            return;
        }
        let state = self.clone();
        let task = self.begin_task("trash", "Limpiando temporales");
        dioxus::core::spawn_forever(async move {
            let core = state.services.core.clone();
            let result = tokio::task::spawn_blocking(move || core.clean_temporary_files()).await;
            drop(task);
            match result {
                Ok(Ok(report)) => {
                    let summary = format!(
                        "{} temporales eliminados · {} liberados",
                        report.files_removed,
                        size_label(report.bytes_removed)
                    );
                    if report.errors.is_empty() {
                        state.notify_success(summary);
                    } else {
                        state.notify_warning(format!(
                            "{summary}; {} archivos no se pudieron limpiar",
                            report.errors.len()
                        ));
                    }
                }
                Ok(Err(error)) => {
                    state.notify_error(format!("No se pudieron limpiar los temporales: {error}"))
                }
                Err(error) => {
                    state.notify_error(format!("No se pudieron limpiar los temporales: {error}"))
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
            let task = state.begin_task("playlist", format!("Exportando «{}»", playlist.name));
            let core = state.services.core.clone();
            let path = output.path().to_owned();
            let result =
                tokio::task::spawn_blocking(move || core.export_playlist_m3u8(&playlist.id, &path))
                    .await;
            drop(task);
            match result {
                Ok(Ok(report)) if report.skipped.is_empty() => state.notify_success(format!(
                    "Playlist exportada: {} pistas",
                    report.exported_tracks
                )),
                Ok(Ok(report)) => state.notify_warning(format!(
                    "Playlist exportada: {} pistas; {} archivos no disponibles",
                    report.exported_tracks,
                    report.skipped.len()
                )),
                Ok(Err(error)) => state.notify_error(format!("No se pudo exportar: {error}")),
                Err(error) => state.notify_error(format!("No se pudo exportar: {error}")),
            }
        });
    }

    pub async fn import_paths(&self, paths: Vec<PathBuf>) {
        let mut busy = self.busy;
        busy.set(true);
        let task = self
            .begin_task("folder-input", "Importando a la biblioteca")
            .page(Page::Library);
        task.detail(match paths.as_slice() {
            [path] => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            paths => format!("{} elementos", paths.len()),
        });
        let core = self.services.core.clone();
        let mode = if self.settings.read().import_mode == "reference" {
            ImportMode::Reference
        } else {
            ImportMode::Copy
        };
        let result = tokio::task::spawn_blocking(move || core.import_paths(&paths, mode)).await;
        busy.set(false);
        drop(task);
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
                if report.errors.is_empty() {
                    self.announce(
                        crate::activity::Tone::Success,
                        format!(
                            "{} archivos importados{}",
                            report.tracks.len(),
                            if report.playlists.is_empty() {
                                String::new()
                            } else {
                                format!(" · {} playlists", report.playlists.len())
                            }
                        ),
                        Some(Page::Library),
                    );
                } else {
                    self.announce(
                        crate::activity::Tone::Warning,
                        format!(
                            "{} importados; {} errores: {errors}",
                            report.tracks.len(),
                            report.errors.len()
                        ),
                        Some(Page::Library),
                    );
                }
            }
            Ok(Err(e)) => self.notify_error(e.to_string()),
            Err(e) => self.notify_error(e.to_string()),
        }
    }

    /// Reconcile the library with its folder in the background. Problems always
    /// produce a notice; success only when the user asked for the scan.
    pub fn scan_library(&self, report_success: bool) {
        let state = self.clone();
        let task = self
            .begin_task("music-2", "Revisando la biblioteca")
            .page(Page::Library);
        dioxus::core::spawn_forever(async move {
            let core = state.services.core.clone();
            let result = tokio::task::spawn_blocking(move || core.scan_library()).await;
            drop(task);
            match result {
                Ok(Ok(report)) => {
                    state.refresh();
                    if !report.errors.is_empty() {
                        state.announce(
                            crate::activity::Tone::Warning,
                            format!(
                                "La revisión de la biblioteca encontró {} archivos que no se pudieron importar",
                                report.errors.len()
                            ),
                            Some(Page::Library),
                        );
                    } else if report_success {
                        state.notify_success("Biblioteca actualizada");
                    }
                }
                Ok(Err(error)) => {
                    state.notify_error(format!("No se pudo revisar la biblioteca: {error}"))
                }
                Err(error) => state.notify_error(format!(
                    "La revisión de la biblioteca se interrumpió: {error}"
                )),
            }
        });
    }

    pub fn start_polling(&self) {
        self.plugin_event(
            "app.started",
            serde_json::json!({"version":env!("CARGO_PKG_VERSION")}),
        );
        if self.settings.peek().scan_library_on_startup {
            self.scan_library(false);
        }
        self.start_library_analysis();
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
                                s.notify_error(error.clone());
                            }
                            s.playing.set(false);
                            s.recover_remote(error.clone());
                        } else if was_playing && snapshot.finished {
                            s.next(true);
                        }
                        s.observe_sound(snapshot.analysis);
                        if *s.blend.peek() != snapshot.blend {
                            if snapshot.blend.is_none() && s.mixing_as.peek().is_some() {
                                s.mixing_as.set(None);
                            }
                            if snapshot.blend.is_none() && s.blend_scene.peek().is_some() {
                                s.blend_scene.set(None);
                            }
                            s.blend.set(snapshot.blend);
                        }
                        last_audio_error = snapshot.error;
                    }
                }
                let timer_expired = s.sleep_at.peek().is_some_and(|at| at <= Instant::now());
                if timer_expired {
                    s.expire_sleep_timer();
                }
                s.activity.expire(Instant::now());
                s.listening_tick();
                s.dj_tick();
                s.publish_next_mix();
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
            Some(error) => state.notify_error(format!(
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
        let (current, queue, _) = restore_playback(&core, &playback);
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

    #[tokio::test]
    async fn remote_selection_is_persisted_before_resolution_and_after_immediate_cancel() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let first = services.core.import_bytes("first.mp4", &[1, 2, 3]).unwrap();
        let capture: CapturedState = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(action_harness);
        dom.insert_any_root_context(Box::new(services.clone()));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let state = capture.borrow().as_ref().unwrap().clone();
        let remote = crate::remote::track_from_result(
            yt_media::SearchResult {
                id: "aqz-KE-bpKQ".into(),
                url: "https://www.youtube.com/watch?v=aqz-KE-bpKQ".into(),
                title: "Pending remote".into(),
                thumbnail: String::new(),
                duration_seconds: 635,
                channel: String::new(),
                view_count: 0,
            },
            true,
        );
        dom.in_scope(ScopeId::ROOT, || {
            state.play(first.clone());
            state.start_remote(remote.clone(), 41.5);
            let saved = services.core.load_playback().unwrap();
            assert_eq!(saved.queue, vec![playback_key(&remote)]);
            assert_eq!(saved.session_history, vec![first.id.clone()]);
            assert_eq!(saved.position_seconds, 41.5);
            state.toggle_play();
            assert!(!*state.stream_loading.peek());
            assert_eq!(services.core.load_playback().unwrap(), saved);
        });
        // Never poll the network task: the selection and cancellation must have
        // reached SQLite synchronously, without waiting for a periodic timer.
        let saved = services.core.load_playback().unwrap();
        assert_eq!(
            restore_playback(&services.core, &saved).0.unwrap().id,
            remote.id
        );
    }

    #[tokio::test]
    async fn persisted_remote_modes_remain_distinct_in_current_queue_and_history() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let capture: CapturedState = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(action_harness);
        dom.insert_any_root_context(Box::new(services.clone()));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let mut state = capture.borrow().as_ref().unwrap().clone();
        let audio = crate::remote::track_from_result(
            yt_media::SearchResult {
                id: "aqz-KE-bpKQ".into(),
                url: "https://www.youtube.com/watch?v=aqz-KE-bpKQ".into(),
                title: "Same content, two modes".into(),
                thumbnail: String::new(),
                duration_seconds: 635,
                channel: String::new(),
                view_count: 0,
            },
            false,
        );
        let mut video = audio.clone();
        video.media_type = "video".into();
        video.extension = "mp4".into();
        dom.in_scope(ScopeId::ROOT, || {
            state.current.set(Some(audio.clone()));
            state.record_session_departure(&video);
            state.current.set(Some(video.clone()));
            state
                .queue
                .set(vec![audio.clone(), video.clone(), audio.clone()]);
            state.persist_playback();
        });
        let saved = services.core.load_playback().unwrap();
        assert_eq!(
            saved.queue,
            vec![
                playback_key(&video),
                playback_key(&audio),
                playback_key(&video),
                playback_key(&audio)
            ]
        );
        let (current, queue, history) = restore_playback(&services.core, &saved);
        assert_eq!(current.unwrap().media_type, "video");
        assert_eq!(
            queue
                .iter()
                .map(|track| track.media_type.as_str())
                .collect::<Vec<_>>(),
            vec!["audio", "video", "audio"]
        );
        assert_eq!(history[0].media_type, "audio");
        assert!(queue.iter().all(|track| track.id == audio.id));
        assert!(services.core.library("").unwrap().is_empty());
        let mut legacy = saved;
        legacy.queue = vec![video.id.clone()];
        legacy.session_history.clear();
        assert_eq!(
            restore_playback(&services.core, &legacy)
                .0
                .unwrap()
                .media_type,
            "video"
        );
        legacy.queue = vec![format!("{}:invalid-mode", video.id)];
        assert!(restore_playback(&services.core, &legacy).0.is_none());
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
    async fn session_back_stack_restores_forward_queue_and_late_repeat_all() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let tracks = ["A.mp4", "B.mp4", "C.mp4"]
            .map(|name| services.core.import_bytes(name, &[1, 2, 3]).unwrap());
        let capture: CapturedState = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(action_harness);
        dom.insert_any_root_context(Box::new(services.clone()));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let mut state = capture.borrow().as_ref().unwrap().clone();
        dom.in_scope(ScopeId::ROOT, || {
            state.play_all(tracks.to_vec(), false);
            state.next(false);
            state.next(false);
            assert_eq!(state.current.peek().as_ref().unwrap().id, tracks[2].id);
            assert_eq!(
                state
                    .session_history
                    .peek()
                    .iter()
                    .map(|t| &t.id)
                    .collect::<Vec<_>>(),
                vec![&tracks[0].id, &tracks[1].id]
            );
            state.previous();
            assert_eq!(state.current.peek().as_ref().unwrap().id, tracks[1].id);
            state.previous();
            assert_eq!(state.current.peek().as_ref().unwrap().id, tracks[0].id);
            assert!(state.session_history.peek().is_empty());
            assert_eq!(
                state.queue.peek().iter().map(|t| &t.id).collect::<Vec<_>>(),
                vec![&tracks[1].id, &tracks[2].id]
            );
            state.next(false);
            assert_eq!(state.current.peek().as_ref().unwrap().id, tracks[1].id);
            state.next(false);
            assert_eq!(state.current.peek().as_ref().unwrap().id, tracks[2].id);
            state.repeat.set(1);
            state.next(true);
            assert_eq!(state.current.peek().as_ref().unwrap().id, tracks[0].id);
            assert!(state.session_history.peek().is_empty());
            assert_eq!(
                state.queue.peek().iter().map(|t| &t.id).collect::<Vec<_>>(),
                vec![&tracks[1].id, &tracks[2].id]
            );
            state.next(true);
            state.next(true);
            state.next(true);
            assert_eq!(state.current.peek().as_ref().unwrap().id, tracks[0].id);
            state.play_all(vec![tracks[2].clone()], false);
            assert!(state.session_history.peek().is_empty());
            state.previous();
            assert_eq!(state.current.peek().as_ref().unwrap().id, tracks[2].id);
            assert_eq!(
                services.core.load_playback().unwrap().session_history,
                Vec::<String>::new()
            );
        });
    }

    #[tokio::test]
    async fn session_history_persists_remote_tracks_and_validates_them_on_restore() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let current = services
            .core
            .import_bytes("current.mp4", &[1, 2, 3])
            .unwrap();
        let remote = crate::remote::track_from_result(
            yt_media::SearchResult {
                id: "aqz-KE-bpKQ".into(),
                url: "https://www.youtube.com/watch?v=aqz-KE-bpKQ".into(),
                title: "Remote history".into(),
                thumbnail: String::new(),
                duration_seconds: 635,
                channel: "Channel".into(),
                view_count: 0,
            },
            false,
        );
        let capture: CapturedState = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(action_harness);
        dom.insert_any_root_context(Box::new(services.clone()));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let mut state = capture.borrow().as_ref().unwrap().clone();
        dom.in_scope(ScopeId::ROOT, || {
            state.current.set(Some(remote.clone()));
            state.play(current.clone());
            assert_eq!(state.session_history.peek()[0].id, remote.id);
        });
        let mut saved = services.core.load_playback().unwrap();
        assert_eq!(saved.session_history, vec![playback_key(&remote)]);
        assert!(saved
            .remote_tracks
            .iter()
            .any(|track| track.id == remote.id));
        let (restored, _, history) = restore_playback(&services.core, &saved);
        assert_eq!(restored.unwrap().id, current.id);
        assert_eq!(history[0].source_url, remote.source_url);
        assert_eq!(services.core.library("").unwrap().len(), 1);
        saved.remote_tracks[0].source_url = "https://example.invalid/watch?v=aqz-KE-bpKQ".into();
        assert!(restore_playback(&services.core, &saved).2.is_empty());
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
            state.session_history.set(vec![third.clone()]);
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
                .last_notice()
                .unwrap()
                .contains("No se pudo reproducir"));
        });
    }
}
