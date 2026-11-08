//! Durable download jobs. Signed URLs and connection credentials stay in memory;
//! all partial media remains outside the managed library.
use crate::state::{download_view, AppState};
use dioxus::prelude::*;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant},
};
use yt_core::{AppCore, ImportMode, Job, JobStatus, MediaMetadata, Settings, Track};
use yt_media::{
    CancellationToken, Downloader, ResumeKey, SearchResult, StreamKind, VideoQuality, YoutubeClient,
};

type DownloadResult<T> = Result<T, String>;
#[derive(Clone)]
struct Preferences {
    video_quality: String,
    audio_quality: u16,
    filename_template: String,
    preserve_metadata: bool,
    thumbnail: bool,
    subtitles: bool,
    language: String,
}
impl Preferences {
    fn current(settings: &Settings) -> Self {
        Self {
            video_quality: settings.video_quality.clone(),
            audio_quality: settings.audio_quality.parse().unwrap_or(320),
            filename_template: settings.filename_template.clone(),
            preserve_metadata: settings.preserve_source_metadata,
            thumbnail: settings.auto_thumbnail,
            subtitles: settings.download_subtitles,
            language: settings.language.clone(),
        }
    }
    fn value(&self) -> Value {
        json!({"video_quality":self.video_quality,"audio_quality":self.audio_quality,"filename_template":self.filename_template,
            "preserve_metadata":self.preserve_metadata,"thumbnail":self.thumbnail,"subtitles":self.subtitles,"language":self.language})
    }
    fn from_value(value: &Value, fallback: &Settings) -> Self {
        let mut prefs = Self::current(fallback);
        if let Some(v) = value["video_quality"].as_str() {
            prefs.video_quality = v.into();
        }
        if let Some(v) = value["audio_quality"]
            .as_u64()
            .filter(|v| [128, 192, 256, 320].contains(v))
        {
            prefs.audio_quality = v as u16;
        }
        if let Some(v) = value["filename_template"].as_str() {
            prefs.filename_template = v.into();
        }
        if let Some(v) = value["preserve_metadata"].as_bool() {
            prefs.preserve_metadata = v;
        }
        if let Some(v) = value["thumbnail"].as_bool() {
            prefs.thumbnail = v;
        }
        if let Some(v) = value["subtitles"].as_bool() {
            prefs.subtitles = v;
        }
        if let Some(v) = value["language"].as_str() {
            prefs.language = v.into();
        }
        prefs
    }
    fn quality(&self) -> DownloadResult<VideoQuality> {
        if self.video_quality == "best" {
            return Ok(VideoQuality::Best);
        }
        self.video_quality
            .parse::<u32>()
            .ok()
            .filter(|v| (144..=4320).contains(v))
            .map(VideoQuality::MaxHeight)
            .ok_or_else(|| "La resolución elegida no es válida.".into())
    }
}
#[derive(Clone)]
struct Update {
    percent: f64,
    detail: String,
}
#[derive(Clone)]
struct Progress(tokio::sync::mpsc::Sender<Update>);
impl Progress {
    fn report(&self, percent: f64, detail: impl Into<String>) {
        let _ = self.0.try_send(Update {
            percent: percent.clamp(0.0, 99.9),
            detail: detail.into(),
        });
    }
}
struct ReadyMedia {
    track: Track,
    warnings: Vec<String>,
    height: u32,
    subtitle_files: Vec<PathBuf>,
}

fn transfer_status(downloaded: u64, total: u64, transferred: u64, elapsed: Duration) -> String {
    let mut text = format!(
        "{:.1} / {:.1} MB",
        downloaded as f64 / 1048576.0,
        total as f64 / 1048576.0
    );
    if elapsed >= Duration::from_millis(250) && transferred > 0 {
        let rate = transferred as f64 / elapsed.as_secs_f64();
        let seconds = (total.saturating_sub(downloaded) as f64 / rate).ceil() as u64;
        text.push_str(&format!(
            " · {:.1} MB/s · quedan {}:{:02}",
            rate / 1048576.0,
            seconds / 60,
            seconds % 60
        ));
    }
    text
}

// Serializes identical output requests before taking a network slot. Repeated
// playlist entries and double clicks reuse the first completed local artifact.
fn source_lock(
    item: &SearchResult,
    video: bool,
    prefs: &Preferences,
) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    let key = format!("{}|{video}|{}", item.url, prefs.value());
    if let Some(existing) = locks.get(&key).and_then(Weak::upgrade) {
        return existing;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

impl AppState {
    fn save_download_progress(
        &self,
        id: &str,
        status: JobStatus,
        progress: f64,
        message: &str,
        result: &Value,
        error: &str,
    ) -> DownloadResult<()> {
        let job = self
            .services
            .core
            .update_job(id, status, progress, message, result, error)
            .map_err(|e| e.to_string())?;
        let mut rows = self.downloads;
        if let Some(row) = rows.write().iter_mut().find(|row| row.id == id) {
            *row = download_view(&job);
        }
        Ok(())
    }
    pub fn download(&self, item: SearchResult, video: bool, play_when_ready: bool) {
        if play_when_ready {
            self.play_remote(item, video);
            return;
        }
        self.enqueue_download(item, video, None, None, None);
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
        let supplied = items.len();
        let mut seen = HashSet::new();
        let items: Vec<_> = items
            .into_iter()
            .filter(|item| seen.insert(item.url.clone()))
            .collect();
        let duplicates = supplied - items.len();
        let order = Arc::new(items.iter().map(|i| i.url.clone()).collect::<Vec<_>>());
        self.plugin_event("playlist.queued",json!({"playlist_id":playlist.id,"title":playlist.name,"total":items.len(),"video":video,"duplicates_reused":duplicates}));
        for item in items {
            self.enqueue_download(
                item,
                video,
                Some(playlist.id.clone()),
                Some(order.clone()),
                None,
            );
        }
        self.refresh();
        self.notify(format!(
            "{} descargas añadidas a {}{}",
            order.len(),
            playlist.name,
            if duplicates > 0 {
                format!("; {duplicates} repetidos se reutilizan")
            } else {
                String::new()
            }
        ));
    }
    /// Retains committed ranges. Reintentar creates a new history row that uses
    /// the same cache even after the application has restarted.
    pub fn pause_download(&self, id: &str) {
        if let Some(token) = self.cancellations.read().get(id) {
            token.cancel();
        }
    }
    pub fn retry_download(&self, id: &str) {
        let Ok(job) = self.services.core.job(id) else {
            return;
        };
        if !job.status.is_terminal() || job.status == JobStatus::Completed {
            return;
        }
        let original_cache = cache_id(&job);
        if self
            .services
            .core
            .jobs_for_download_cache(&original_cache)
            .is_ok_and(|jobs| jobs.iter().any(|j| !j.status.is_terminal()))
        {
            self.notify("Esta descarga ya está en la cola");
            return;
        }
        let item = item_from_payload(&job.payload);
        let playlist = job.payload["playlist_id"].as_str().map(str::to_owned);
        let order = serde_json::from_value::<Vec<String>>(job.payload["playlist_order"].clone())
            .ok()
            .map(Arc::new);
        self.enqueue_download(
            item,
            job.payload["video"].as_bool().unwrap_or(false),
            playlist,
            order,
            Some(job),
        );
    }
    /// Remove history without discarding a cache still referenced by another
    /// retry. Complete media in the library is independent of job history.
    pub fn remove_download_history(&self, id: &str) {
        match remove_history_record(&self.services.core, &self.services.profile, id) {
            Ok(()) => {
                let mut rows = self.downloads;
                rows.write().retain(|row| row.id != id);
            }
            Err(error) => self.notify(error),
        }
    }
    /// Active jobs are interrupted first; cleanup occurs after their writer has
    /// released its lock. Complete library files are never removed.
    pub fn discard_download(&self, id: &str) {
        let Ok(job) = self.services.core.job(id) else {
            return;
        };
        if !job.status.is_terminal() {
            let mut result = job.result.clone();
            if !result.is_object() {
                result = json!({});
            }
            result["discard_requested"] = json!(true);
            if let Err(e) = self.save_download_progress(
                id,
                job.status,
                job.progress,
                "Cancelando y descartando descarga parcial",
                &result,
                "",
            ) {
                self.notify(e);
                return;
            }
            self.pause_download(id);
            return;
        }
        let cache_key = cache_id(&job);
        if self
            .services
            .core
            .jobs_for_download_cache(&cache_key)
            .is_ok_and(|jobs| jobs.iter().any(|j| !j.status.is_terminal()))
        {
            self.notify("La descarga parcial está siendo usada por otro intento");
            return;
        }
        match remove_cache(
            &self.services.profile,
            &cache_key,
            &self.services.library_dir,
        ) {
            Ok(()) => {
                let _ = self.save_download_progress(
                    id,
                    job.status,
                    job.progress,
                    "Descarga parcial descartada",
                    &json!({"discarded":true}),
                    "",
                );
            }
            Err(error) => self.notify(error),
        }
    }
    fn enqueue_download(
        &self,
        mut item: SearchResult,
        video: bool,
        playlist: Option<String>,
        order: Option<Arc<Vec<String>>>,
        previous: Option<Job>,
    ) {
        let url = match yt_media::canonical_video_url(&item.url) {
            Ok(v) => v,
            Err(e) => {
                self.notify(e.to_string());
                return;
            }
        };
        item.url = url;
        let prefs = previous
            .as_ref()
            .map(|j| Preferences::from_value(&j.payload["preferences"], &self.settings.peek()))
            .unwrap_or_else(|| Preferences::current(&self.settings.peek()));
        if let Err(error) = render_name(
            &prefs.filename_template,
            &item,
            video,
            if video { &prefs.video_quality } else { "192" },
        ) {
            self.notify(error);
            return;
        }
        let payload = json!({"url":item.url,"title":item.title,"video":video,"thumbnail":item.thumbnail,"channel":item.channel,"media_id":item.id,
            "duration_seconds":item.duration_seconds,"playlist_id":playlist,"playlist_order":order.as_deref(),"preferences":prefs.value(),
            "cache_id":previous.as_ref().map(cache_id),"retry_of":previous.as_ref().map(|j|j.id.clone())});
        let job = match self.services.core.create_job("download", &payload) {
            Ok(j) => j,
            Err(e) => {
                self.notify(format!("No se pudo iniciar la descarga: {e}"));
                return;
            }
        };
        let mut s = self.clone();
        let id = job.id.clone();
        let cache_key = cache_id(&job);
        let cancel = CancellationToken::new();
        s.cancellations.write().insert(id.clone(), cancel.clone());
        s.downloads.write().insert(0, download_view(&job));
        s.plugin_event("download.queued",json!({"job_id":id,"url":item.url,"title":item.title,"video":video,"playlist_id":playlist}));
        dioxus::core::spawn_forever(async move {
            let (updates, mut incoming) = tokio::sync::mpsc::channel(24);
            let progress = Progress(updates);
            let work_state = s.clone();
            let serial = source_lock(&item, video, &prefs);
            let work = async {
                let _source = tokio::select! {_=cancel.cancelled()=>return Err("La operación fue cancelada".to_owned()),guard=serial.lock()=>guard};
                let _slot = work_state.services.download_slots.acquire(&cancel).await?;
                work_state.save_download_progress(
                    &id,
                    JobStatus::Running,
                    0.0,
                    "Preparando descarga",
                    &json!({}),
                    "",
                )?;
                let youtube = work_state
                    .services
                    .youtube_client()
                    .map_err(|e| e.to_string())?;
                let downloader = work_state.services.downloader.borrow().clone();
                let retries = work_state.settings.peek().download_retries;
                run_download(
                    &work_state.services.core,
                    &work_state.services.profile,
                    &work_state.services.library_dir,
                    &cache_key,
                    &item,
                    video,
                    &prefs,
                    &youtube,
                    &downloader,
                    retries,
                    &cancel,
                    &progress,
                )
                .await
            };
            tokio::pin!(work);
            let mut last_saved = Instant::now();
            let result = loop {
                tokio::select! {
                    result=&mut work=>break result,
                    Some(update)=incoming.recv()=>{
                        if let Some(row)=s.downloads.write().iter_mut().find(|r|r.id==id){row.status="Descargando".into();row.progress=update.percent;row.detail=update.detail.clone();}
                        if last_saved.elapsed()>=Duration::from_secs(1){
                            let retained=s.services.core.job(&id).map(|j|j.result).unwrap_or_else(|_|json!({}));
                            let _=s.save_download_progress(&id,JobStatus::Running,update.percent,&update.detail,&retained,"");
                            s.plugin_event("download.progress",json!({"job_id":id,"progress":update.percent,"message":update.detail}));last_saved=Instant::now();
                        }
                    }
                }
            };
            s.cancellations.write().remove(&id);
            match result {
                Ok(ready) => {
                    if let Some(playlist) = &playlist {
                        if let Err(error) = attach_playlist(
                            &s.services.core,
                            playlist,
                            &ready.track.id,
                            order.as_deref().map(Vec::as_slice),
                        ) {
                            s.notify(format!("El archivo se guardó, pero no se pudo actualizar la playlist: {error}"));
                        }
                        let complete = s
                            .services
                            .core
                            .playlist_tracks(playlist)
                            .map(|tracks| {
                                tracks
                                    .iter()
                                    .filter(|t| {
                                        order.as_ref().is_none_or(|o| o.contains(&t.source_url))
                                    })
                                    .count()
                            })
                            .unwrap_or(0);
                        s.plugin_event("playlist.progress",json!({"playlist_id":playlist,"completed":complete,"total":order.as_ref().map(|o|o.len())}));
                    }
                    let result = json!({"track_id":ready.track.id,"height":ready.height,"audio_bitrate":prefs.audio_quality,"warnings":ready.warnings,"subtitles":ready.subtitle_files});
                    if let Err(e) = s.save_download_progress(
                        &id,
                        JobStatus::Completed,
                        100.0,
                        &if ready.height > 0 {
                            format!("Descarga completada · {}p", ready.height)
                        } else {
                            "Descarga completada".into()
                        },
                        &result,
                        "",
                    ) {
                        s.notify(format!(
                            "El archivo se guardó, pero su historial no pudo actualizarse: {e}"
                        ));
                    }
                    s.plugin_event(
                        "download.progress",
                        json!({"job_id":id,"progress":100.0,"status":"completed"}),
                    );
                    s.plugin_event("download.media_ready",json!({"job_id":id,"track_id":ready.track.id,"path":ready.track.path,"url":ready.track.source_url,"video":video,"height":ready.height}));
                    let _ = remove_cache(&s.services.profile, &cache_key, &s.services.library_dir);
                    s.refresh();
                    if !ready.warnings.is_empty() {
                        s.notify(format!("Descarga completada. {}", ready.warnings.join(" ")));
                    }
                }
                Err(error) => {
                    let discard = s
                        .services
                        .core
                        .job(&id)
                        .ok()
                        .is_some_and(|j| j.result["discard_requested"] == true);
                    let cleanup = if discard {
                        remove_cache(&s.services.profile, &cache_key, &s.services.library_dir).err()
                    } else {
                        None
                    };
                    let status = if cancel.is_cancelled() {
                        JobStatus::Canceled
                    } else {
                        JobStatus::Error
                    };
                    let detail = if discard {
                        "Descarga cancelada; parcial descartado".to_owned()
                    } else if cancel.is_cancelled() {
                        "Descarga pausada; Reintentar continúa los bytes guardados".into()
                    } else {
                        error.clone()
                    };
                    let percent = s
                        .downloads
                        .peek()
                        .iter()
                        .find(|r| r.id == id)
                        .map(|r| r.progress)
                        .unwrap_or(0.0);
                    if let Err(e) = s.save_download_progress(
                        &id,
                        status,
                        percent,
                        &detail,
                        &json!({"resumable":!discard,"discarded":discard}),
                        if cancel.is_cancelled() { "" } else { &error },
                    ) {
                        s.notify(e);
                    }
                    if let Some(e) = cleanup {
                        s.notify(format!("No se pudo limpiar el parcial: {e}"));
                    } else if !cancel.is_cancelled() {
                        s.notify(format!("No se pudo descargar: {error}"));
                    }
                }
            }
        });
    }
}

fn remove_history_record(core: &AppCore, profile: &Path, id: &str) -> DownloadResult<()> {
    let job = core.job(id).map_err(|e| e.to_string())?;
    if !job.status.is_terminal() {
        return Err("Pausa la descarga antes de quitarla del historial".into());
    }
    let key = cache_id(&job);
    let referenced = core
        .jobs_for_download_cache(&key)
        .map_err(|e| e.to_string())?
        .iter()
        .any(|other| other.id != id && cache_id(other) == key && other.result["discarded"] != true);
    if !referenced {
        remove_cache(profile, &key, core.library_dir())?;
    }
    core.remove_job(id).map_err(|e| e.to_string())
}
fn saved_output(core: &AppCore, track: &Track, jobs: &[Job]) -> Option<Value> {
    core.setting(&format!("rust.download.{}",track.id)).ok().flatten().or_else(||jobs.iter().find(|j|j.status==JobStatus::Completed && j.result["track_id"].as_str()==Some(&track.id))
        .map(|j|json!({"preferences":j.payload["preferences"],"height":j.result["height"],"subtitles":j.result["subtitles"]})))
}

fn cache_id(job: &Job) -> String {
    job.payload["cache_id"]
        .as_str()
        .filter(|v| valid_cache_id(v))
        .unwrap_or(&job.id)
        .into()
}
fn valid_cache_id(value: &str) -> bool {
    value.len() == 36 && value.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}
fn cache_directory(profile: &Path, id: &str) -> DownloadResult<PathBuf> {
    if !valid_cache_id(id) {
        return Err("Identidad de caché no válida".into());
    }
    let root = profile.join("download-cache");
    if root.try_exists().map_err(|e| e.to_string())?
        && !std::fs::symlink_metadata(&root)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_dir()
    {
        return Err("La caché de descargas no es una carpeta segura".into());
    }
    let path = root.join(id);
    if path.try_exists().map_err(|e| e.to_string())?
        && !std::fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_dir()
    {
        return Err("La descarga parcial no es una carpeta segura".into());
    }
    Ok(path)
}
fn remove_cache(profile: &Path, id: &str, library: &Path) -> DownloadResult<()> {
    let path = cache_directory(profile, id)?;
    let lock = lock_cache(&path)?;
    if !path.exists() {
        return Ok(());
    }
    // Older versions kept the owner handle inside the directory. Check it, but
    // do not retain an open child while renaming its parent on Windows.
    if path.join("owner.lock").exists() {
        let legacy = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.join("owner.lock"))
            .map_err(|e| e.to_string())?;
        legacy
            .try_lock()
            .map_err(|_| "La descarga sigue activa".to_owned())?;
    }
    let publication = path.join("publication.json");
    if publication.exists() {
        cleanup_staging(library, &path, &read_json(&publication)?)?;
    }
    let tombstone = tempfile::Builder::new()
        .prefix(".ytu-discard-")
        .tempdir_in(path.parent().ok_or("Caché no válida")?)
        .map_err(|e| e.to_string())?;
    std::fs::rename(&path, tombstone.path().join("partial")).map_err(|e| e.to_string())?;
    drop(lock);
    tombstone.close().map_err(|e| e.to_string())?;
    Ok(())
}

fn lock_cache(directory: &Path) -> DownloadResult<File> {
    let parent = directory.parent().ok_or("Caché no válida")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    // Keep this stable inode outside the deletable cache. Removing a lock file
    // would let concurrent retries lock different inodes for the same cache.
    let path = directory.with_extension("lock");
    if path.exists()
        && !std::fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_file()
    {
        return Err("El bloqueo de la descarga no es un archivo seguro".into());
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    lock.try_lock()
        .map_err(|_| "La descarga sigue activa".to_owned())?;
    Ok(lock)
}
fn item_from_payload(value: &Value) -> SearchResult {
    let text = |key| value[key].as_str().unwrap_or_default().to_owned();
    SearchResult {
        id: text("media_id"),
        url: text("url"),
        title: text("title"),
        thumbnail: text("thumbnail"),
        channel: text("channel"),
        duration_seconds: value["duration_seconds"].as_u64().unwrap_or(0),
        view_count: 0,
    }
}
fn write_json(path: &Path, value: &Value) -> DownloadResult<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))
        .map_err(|e| e.to_string())?;
    serde_json::to_writer(temporary.as_file_mut(), value).map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary.persist(path).map_err(|e| e.error.to_string())?;
    Ok(())
}
fn read_json(path: &Path) -> DownloadResult<Value> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.file_type().is_file() || meta.len() > 2 * 1024 * 1024 {
        return Err("El registro parcial no es válido".into());
    }
    serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
fn check(cancel: &CancellationToken) -> DownloadResult<()> {
    if cancel.is_cancelled() {
        Err("La operación fue cancelada".into())
    } else {
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_download(
    core: &AppCore,
    profile: &Path,
    library: &Path,
    cache_key: &str,
    item: &SearchResult,
    video: bool,
    prefs: &Preferences,
    youtube: &YoutubeClient,
    downloader: &Downloader,
    retries: u8,
    cancel: &CancellationToken,
    progress: &Progress,
) -> DownloadResult<ReadyMedia> {
    check(cancel)?;
    // Reuse a completed source only when it has the requested output settings.
    let jobs = core.jobs().map_err(|e| e.to_string())?;
    let existing = core
        .library("")
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|track| {
            !track.missing
                && track.source_url == item.url
                && (track.media_type == "video") == video
                && saved_output(core, track, &jobs)
                    .is_some_and(|saved| saved["preferences"] == prefs.value())
        });
    if let Some(track) = existing {
        let previous = saved_output(core, &track, &jobs).unwrap_or_default();
        let height = previous["height"].as_u64().unwrap_or(0) as u32;
        let subtitle_files =
            serde_json::from_value(previous["subtitles"].clone()).unwrap_or_default();
        return Ok(ReadyMedia {
            track,
            warnings: Vec::new(),
            height,
            subtitle_files,
        });
    }
    let directory = cache_directory(profile, cache_key)?;
    let lock = lock_cache(&directory)?;
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let plan_path = directory.join("source.json");
    let plan = if plan_path.exists() {
        read_json(&plan_path)?
    } else {
        let plan = if video {
            let resolved = youtube
                .resolve_download(&item.url, prefs.quality()?, cancel.clone())
                .await
                .map_err(|e| e.to_string())?;
            json!({"media":resolved.media,"video":true,"height":resolved.height,"quality_label":resolved.quality_label,
                "primary":ResumeKey::for_media(&resolved.video).map_err(|e|e.to_string())?,"audio":resolved.audio.as_ref().map(ResumeKey::for_media).transpose().map_err(|e|e.to_string())?})
        } else {
            let resolved = tokio::select! {_=cancel.cancelled()=>return Err("La operación fue cancelada".into()),value=youtube.resolve(&item.url,StreamKind::Audio)=>value.map_err(|e|e.to_string())?};
            json!({"media":resolved.media,"video":false,"height":0,"primary":ResumeKey::for_media(&resolved).map_err(|e|e.to_string())?,"audio":null})
        };
        write_json(&plan_path, &plan)?;
        plan
    };
    let media: SearchResult =
        serde_json::from_value(plan["media"].clone()).map_err(|e| e.to_string())?;
    if media.url != item.url || plan["video"].as_bool() != Some(video) {
        return Err("El parcial pertenece a otro contenido".into());
    }
    let primary: ResumeKey =
        serde_json::from_value(plan["primary"].clone()).map_err(|e| e.to_string())?;
    let audio: Option<ResumeKey> =
        serde_json::from_value(plan["audio"].clone()).map_err(|e| e.to_string())?;
    let primary_path = directory.join(if video { "video.mp4" } else { "audio.m4a" });
    let audio_path = directory.join("audio.m4a");
    if primary.media_id != media.id || audio.as_ref().is_some_and(|k| k.media_id != media.id) {
        return Err("Las pistas parciales pertenecen a otro vídeo".into());
    }
    let total = primary
        .total_bytes
        .checked_add(audio.as_ref().map_or(0, |k| k.total_bytes))
        .filter(|v| *v > 0)
        .ok_or("Tamaño de descarga inválido")?;
    transfer(
        youtube,
        downloader,
        &media.url,
        &primary_path,
        &primary,
        retries,
        cancel,
        progress,
        0,
        total,
    )
    .await?;
    if let Some(key) = &audio {
        transfer(
            youtube,
            downloader,
            &media.url,
            &audio_path,
            key,
            retries,
            cancel,
            progress,
            primary.total_bytes,
            total,
        )
        .await?;
    }
    let mut warnings = Vec::new();
    let mut artwork = None;
    if prefs.thumbnail && prefs.preserve_metadata && !media.thumbnail.is_empty() {
        progress.report(83.0, "Obteniendo carátula");
        match youtube
            .fetch_thumbnail(&media.thumbnail, cancel.clone())
            .await
        {
            Ok(bytes) => match core.cache_artwork(&bytes) {
                Ok(path) => artwork = Some((path, bytes)),
                Err(e) => warnings.push(format!("No se guardó la carátula: {e}")),
            },
            Err(e) => {
                check(cancel)?;
                warnings.push(format!("No se pudo obtener la carátula: {e}"));
            }
        }
    }
    let prepared = directory.join(if video { "ready.mp4" } else { "ready.mp3" });
    let already_prepared = prepared.exists();
    if !already_prepared {
        if video {
            if audio.is_some() {
                progress.report(85.0, "Uniendo vídeo y audio");
                let video_path = primary_path.clone();
                let audio_path = audio_path.clone();
                let output = prepared.clone();
                let token = cancel.clone();
                let updates = progress.clone();
                tokio::task::spawn_blocking(move || {
                    yt_media::mux_mp4(&video_path, &audio_path, &output, token, move |p| {
                        updates.report(85.0 + p.fraction() * 8.0, "Uniendo vídeo y audio")
                    })
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            } else {
                let source = primary_path.clone();
                let output = prepared.clone();
                let token = cancel.clone();
                tokio::task::spawn_blocking(move || copy_complete(&source, &output, &token))
                    .await
                    .map_err(|e| e.to_string())??;
            }
        } else {
            progress.report(85.0, "Convirtiendo a MP3");
            let metadata = MediaMetadata {
                title: Some(media.title.clone()),
                artist: Some(if prefs.preserve_metadata {
                    media.channel.clone()
                } else {
                    String::new()
                }),
                source_url: Some(media.url.clone()),
                artwork_path: artwork.as_ref().map(|(p, _)| p.clone()),
                preserve_source_tags: prefs.preserve_metadata,
                ..Default::default()
            };
            let source = primary_path.clone();
            let output = prepared.clone();
            let token = cancel.clone();
            let bitrate = prefs.audio_quality;
            let updates = progress.clone();
            tokio::task::spawn_blocking(move || {
                yt_audio::convert_with_metadata(
                    &source,
                    &output,
                    yt_audio::AudioFormat::Mp3(bitrate),
                    &metadata,
                    &token,
                    move |fraction| updates.report(85.0 + fraction * 8.0, "Convirtiendo a MP3"),
                )
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        }
    }
    if already_prepared && !video && artwork.is_some() {
        let metadata = MediaMetadata {
            title: Some(media.title.clone()),
            artist: Some(if prefs.preserve_metadata {
                media.channel.clone()
            } else {
                String::new()
            }),
            source_url: Some(media.url.clone()),
            artwork_path: artwork.as_ref().map(|(path, _)| path.clone()),
            preserve_source_tags: prefs.preserve_metadata,
            ..Default::default()
        };
        let input = primary_path.clone();
        let output = prepared.clone();
        let token = cancel.clone();
        tokio::task::spawn_blocking(move || {
            yt_audio::copy_metadata(&input, &output, &metadata, &token)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    }
    check(cancel)?;
    let mut subtitles = Vec::new();
    if prefs.subtitles {
        progress.report(94.0, "Obteniendo subtítulos");
        match youtube.video_details(&media.url, cancel.clone()).await {
            Ok(details) => {
                let selected = details
                    .subtitles
                    .iter()
                    .filter(|t| language_matches(&t.language_code, &prefs.language))
                    .min_by_key(|t| {
                        (
                            t.automatic,
                            !t.language_code.eq_ignore_ascii_case(&prefs.language),
                        )
                    })
                    .or_else(|| {
                        details
                            .subtitles
                            .iter()
                            .find(|t| language_matches(&t.language_code, "en") && !t.automatic)
                    })
                    .or_else(|| details.subtitles.iter().find(|t| !t.automatic))
                    .or(details.subtitles.first());
                if let Some(track) = selected {
                    match youtube
                        .subtitles(&media.url, &track.id, cancel.clone())
                        .await
                    {
                        Ok(document) => {
                            subtitles.push((track.language_code.clone(), document.to_vtt()));
                        }
                        Err(e) => {
                            check(cancel)?;
                            warnings.push(format!("No se guardaron los subtítulos: {e}"));
                        }
                    }
                } else {
                    warnings.push("Este vídeo no ofrece subtítulos.".into());
                }
            }
            Err(e) => {
                check(cancel)?;
                warnings.push(format!("No se consultaron los subtítulos: {e}"));
            }
        }
    }
    check(cancel)?;
    let quality = if video {
        plan["quality_label"]
            .as_str()
            .unwrap_or(&prefs.video_quality)
            .to_owned()
    } else {
        prefs.audio_quality.to_string()
    };
    let name = render_name(&prefs.filename_template, &media, video, &quality)?;
    progress.report(98.0, "Guardando archivo completo");
    let source = prepared.clone();
    let library = library.to_owned();
    let cache = directory.clone();
    let token = cancel.clone();
    let published = tokio::task::spawn_blocking(move || {
        publish_prepared(&source, &library, &name, &cache, &token)
    })
    .await
    .map_err(|e| e.to_string())??;
    // Publication is the commit point: complete indexing even when cancellation
    // arrives now, so a completed file is never abandoned in the library.
    let source_url = media.url.clone();
    let title = media.title.clone();
    let artist = if prefs.preserve_metadata {
        media.channel.clone()
    } else {
        String::new()
    };
    let thumbnail = if prefs.preserve_metadata {
        media.thumbnail.clone()
    } else {
        String::new()
    };
    let indexing_core = core.clone();
    let file = published.clone();
    let track = tokio::task::spawn_blocking(move || {
        let track = indexing_core.import_file(&file, ImportMode::Reference)?;
        indexing_core.update_track_metadata(&track.id, &title, &artist, &thumbnail, &source_url)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    if let Some((_, bytes)) = artwork {
        if let Err(e) = core.set_track_artwork(&track.id, &bytes) {
            warnings.push(format!("La carátula no se asoció a la biblioteca: {e}"));
        }
    }
    let mut subtitle_files = Vec::new();
    for (language, vtt) in subtitles {
        let subtitle = published.with_extension(format!("{}.vtt", safe_language(&language)));
        match publish_bytes(vtt.as_bytes(), &subtitle) {
            Ok(()) => subtitle_files.push(subtitle),
            Err(e) => warnings.push(format!("No se guardó el archivo de subtítulos: {e}")),
        }
    }
    if let Err(error) = core.set_setting(
        &format!("rust.download.{}", track.id),
        &json!({"preferences":prefs.value(),"height":plan["height"],"subtitles":subtitle_files}),
    ) {
        warnings.push(format!(
            "No se guardaron las preferencias del archivo: {error}"
        ));
    }
    drop(lock);
    Ok(ReadyMedia {
        track,
        warnings,
        height: plan["height"].as_u64().unwrap_or(0) as u32,
        subtitle_files,
    })
}

#[allow(clippy::too_many_arguments)]
async fn transfer(
    youtube: &YoutubeClient,
    downloader: &Downloader,
    url: &str,
    destination: &Path,
    key: &ResumeKey,
    retries: u8,
    cancel: &CancellationToken,
    progress: &Progress,
    offset: u64,
    total: u64,
) -> DownloadResult<()> {
    if destination.exists() {
        let meta = std::fs::symlink_metadata(destination).map_err(|e| e.to_string())?;
        if !meta.file_type().is_file() || meta.len() != key.total_bytes {
            return Err("El archivo parcial completo cambió de tamaño".into());
        }
        return Ok(());
    }
    for attempt in 0..=retries {
        check(cancel)?;
        let resolved = youtube.resolve_resume(url, key, cancel.clone()).await;
        let result = match resolved {
            Ok(resolved) => {
                if ResumeKey::for_media(&resolved).map_err(|e| e.to_string())? != *key {
                    return Err("El contenido o formato cambió; descarta el parcial antes de volver a descargar".into());
                }
                let updates = progress.clone();
                let speed = Mutex::new(None::<(u64, Instant)>);
                downloader
                    .download_resumable(
                        &resolved.stream_url,
                        destination,
                        key.clone(),
                        cancel.clone(),
                        move |p| {
                            let mut sample =
                                speed.lock().unwrap_or_else(|error| error.into_inner());
                            let (first, started) =
                                sample.get_or_insert((p.downloaded_bytes, Instant::now()));
                            let detail = transfer_status(
                                offset + p.downloaded_bytes,
                                total,
                                p.downloaded_bytes.saturating_sub(*first),
                                started.elapsed(),
                            );
                            drop(sample);
                            updates.report(
                                82.0 * (offset + p.downloaded_bytes) as f64 / total.max(1) as f64,
                                detail,
                            );
                        },
                    )
                    .await
                    .map(|_| ())
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => return Ok(()),
            Err(error) => {
                check(cancel)?;
                if attempt == retries
                    || !matches!(
                        error,
                        yt_media::MediaError::Network(_)
                            | yt_media::MediaError::Youtube(_)
                            | yt_media::MediaError::IncompleteDownload
                    )
                {
                    return Err(error.to_string());
                }
                let committed = downloader
                    .resume_status(destination)
                    .ok()
                    .flatten()
                    .map_or(0, |s| s.completed_bytes);
                progress.report(
                    82.0 * (offset + committed) as f64 / total.max(1) as f64,
                    format!(
                        "Reintentando conexión ({}/{retries}); se conservan los bytes",
                        attempt + 1
                    ),
                );
                tokio::select! {_=cancel.cancelled()=>return Err("La operación fue cancelada".into()),_=tokio::time::sleep(Duration::from_secs((attempt as u64+1).min(5)))=>{}}
            }
        }
    }
    unreachable!()
}
pub(crate) fn validate_filename_template(template: &str) -> DownloadResult<()> {
    if template.is_empty() || template.len() > 200 {
        return Err("La plantilla debe tener entre 1 y 200 caracteres".into());
    }
    let example = SearchResult {
        id: "abcdefghijk".into(),
        url: String::new(),
        title: "Título".into(),
        channel: "Artista".into(),
        thumbnail: String::new(),
        duration_seconds: 0,
        view_count: 0,
    };
    render_name(template, &example, false, "192").map(|_| ())
}
fn render_name(
    template: &str,
    media: &SearchResult,
    video: bool,
    quality: &str,
) -> DownloadResult<String> {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let mut field = String::new();
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == '}' {
                        closed = true;
                        break;
                    }
                    field.push(c);
                }
                if !closed {
                    return Err("La plantilla de nombre tiene una llave sin cerrar".into());
                }
                let value = match field.as_str() {
                    "title" => &media.title,
                    "channel" => &media.channel,
                    "video_id" => &media.id,
                    "media_type" => {
                        if video {
                            "video"
                        } else {
                            "audio"
                        }
                    }
                    "quality" => quality,
                    _ => return Err(format!("Campo de plantilla no válido: {field}")),
                };
                out.push_str(value);
            }
            '}' => return Err("La plantilla de nombre tiene una llave sin abrir".into()),
            c => out.push(c),
        }
    }
    if out.trim().is_empty() {
        out = media.title.clone();
    }
    Ok(yt_media::safe_file_name(
        &out,
        if video { "mp4" } else { "mp3" },
    ))
}
fn language_matches(code: &str, preferred: &str) -> bool {
    code.eq_ignore_ascii_case(preferred)
        || code
            .split_once('-')
            .is_some_and(|(base, _)| base.eq_ignore_ascii_case(preferred))
}
fn safe_language(language: &str) -> String {
    language
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(32)
        .collect()
}
fn copy_complete(input: &Path, output: &Path, cancel: &CancellationToken) -> DownloadResult<()> {
    let parent = output.parent().ok_or("Destino no válido")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    let mut source = File::open(input).map_err(|e| e.to_string())?;
    let mut block = [0; 128 * 1024];
    loop {
        check(cancel)?;
        let n = source.read(&mut block).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        temp.write_all(&block[..n]).map_err(|e| e.to_string())?;
    }
    temp.as_file().sync_all().map_err(|e| e.to_string())?;
    check(cancel)?;
    temp.persist_noclobber(output)
        .map_err(|e| e.error.to_string())?;
    Ok(())
}
fn same_bytes(first: &Path, second: &Path, cancel: &CancellationToken) -> DownloadResult<bool> {
    if !std::fs::symlink_metadata(second)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_file()
    {
        return Ok(false);
    }
    let mut first = File::open(first).map_err(|e| e.to_string())?;
    let mut second = File::open(second).map_err(|e| e.to_string())?;
    if first.metadata().map_err(|e| e.to_string())?.len()
        != second.metadata().map_err(|e| e.to_string())?.len()
    {
        return Ok(false);
    }
    let mut a = [0; 128 * 1024];
    let mut b = [0; 128 * 1024];
    loop {
        check(cancel)?;
        let n = first.read(&mut a).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(true);
        }
        second.read_exact(&mut b[..n]).map_err(|e| e.to_string())?;
        if a[..n] != b[..n] {
            return Ok(false);
        }
    }
}
// The only staging directory eligible for cleanup is the exact sibling named
// by this cache's durable manifest and carrying its matching ownership marker.
fn cleanup_staging(library: &Path, cache: &Path, saved: &Value) -> DownloadResult<()> {
    let Some(name) = saved["staging"].as_str() else {
        return Ok(());
    };
    if !name.starts_with(".ytu-publishing-")
        || Path::new(name)
            .file_name()
            .is_none_or(|value| value != name)
    {
        return Err("El registro de publicación no es válido".into());
    }
    let path = library.parent().ok_or("Biblioteca no válida")?.join(name);
    if !path.try_exists().map_err(|e| e.to_string())? {
        return Ok(());
    }
    if !std::fs::symlink_metadata(&path)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_dir()
    {
        return Err("La carpeta de publicación fue modificada".into());
    }
    let owner = read_json(&path.join(".ytu-owner.json"))?;
    if owner["cache_id"].as_str() != cache.file_name().and_then(|s| s.to_str()) {
        return Err("La carpeta de publicación pertenece a otro trabajo".into());
    }
    std::fs::remove_dir_all(path).map_err(|e| e.to_string())
}

fn publish_prepared(
    source: &Path,
    library: &Path,
    name: &str,
    cache: &Path,
    cancel: &CancellationToken,
) -> DownloadResult<PathBuf> {
    std::fs::create_dir_all(library).map_err(|e| e.to_string())?;
    let plan = cache.join("publication.json");
    if plan.exists() {
        let saved = read_json(&plan)?;
        cleanup_staging(library, cache, &saved)?;
        if let Some(filename) = saved["filename"]
            .as_str()
            .filter(|v| Path::new(v).file_name().is_some_and(|n| n == *v))
        {
            let previous = library.join(filename);
            if previous.exists() && same_bytes(source, &previous, cancel)? {
                return Ok(previous);
            }
        }
    }
    let stem = Path::new(name)
        .file_stem()
        .and_then(|v| v.to_str())
        .ok_or("Nombre no válido")?;
    let ext = Path::new(name)
        .extension()
        .and_then(|v| v.to_str())
        .ok_or("Extensión no válida")?;
    for index in 0..10000 {
        check(cancel)?;
        let filename = if index == 0 {
            name.to_owned()
        } else {
            format!("{stem} ({index}).{ext}")
        };
        let destination = library.join(&filename);
        if destination.exists() {
            continue;
        }
        // The staging file is outside the library but on the destination volume.
        let parent = library
            .parent()
            .ok_or("La biblioteca no tiene una carpeta contenedora")?;
        let temporary = tempfile::Builder::new()
            .prefix(".ytu-publishing-")
            .tempdir_in(parent)
            .map_err(|e| e.to_string())?;
        write_json(
            &temporary.path().join(".ytu-owner.json"),
            &json!({"cache_id":cache.file_name().and_then(|s|s.to_str())}),
        )?;
        write_json(
            &plan,
            &json!({"filename":filename,"staging":temporary.path().file_name().and_then(|s|s.to_str())}),
        )?;
        let staged = temporary.path().join(name);
        copy_complete(source, &staged, cancel)?;
        check(cancel)?;
        let staged = tempfile::TempPath::try_from_path(staged).map_err(|e| e.to_string())?;
        match staged.persist_noclobber(&destination) {
            Ok(()) => return Ok(destination),
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.error.to_string()),
        }
    }
    Err("No hay un nombre de archivo libre".into())
}
fn publish_bytes(bytes: &[u8], output: &Path) -> DownloadResult<()> {
    if output.exists() {
        if std::fs::read(output).map_err(|e| e.to_string())? == bytes {
            return Ok(());
        }
        return Err("El archivo ya existe y se conservó".into());
    }
    let parent = output
        .parent()
        .and_then(Path::parent)
        .ok_or("Destino no válido")?;
    let mut temp = tempfile::Builder::new()
        .prefix(".ytu-sidecar-")
        .tempfile_in(parent)
        .map_err(|e| e.to_string())?;
    temp.write_all(bytes).map_err(|e| e.to_string())?;
    temp.as_file().sync_all().map_err(|e| e.to_string())?;
    temp.persist_noclobber(output)
        .map_err(|e| e.error.to_string())?;
    Ok(())
}
fn attach_playlist(
    core: &AppCore,
    playlist: &str,
    track: &str,
    order: Option<&[String]>,
) -> DownloadResult<()> {
    core.add_to_playlist(playlist, track)
        .map_err(|e| e.to_string())?;
    if let Some(order) = order {
        let mut tracks = core.playlist_tracks(playlist).map_err(|e| e.to_string())?;
        tracks.sort_by_key(|t| {
            order
                .iter()
                .position(|url| url == &t.source_url)
                .map(|index| (1, index))
                .unwrap_or((0, 0))
        });
        core.reorder_playlist(
            playlist,
            &tracks.into_iter().map(|t| t.id).collect::<Vec<_>>(),
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transfer_rate_uses_new_bytes_and_reports_a_finite_remaining_time() {
        let text = transfer_status(8 * 1048576, 10 * 1048576, 1048576, Duration::from_secs(2));
        assert_eq!(text, "8.0 / 10.0 MB · 0.5 MB/s · quedan 0:04");
        assert_eq!(
            transfer_status(8 * 1048576, 10 * 1048576, 0, Duration::ZERO),
            "8.0 / 10.0 MB"
        );
        assert!(validate_filename_template("{title} - {channel}").is_ok());
        assert!(validate_filename_template("{unknown}").is_err());
    }
    fn media() -> SearchResult {
        SearchResult {
            id: "dQw4w9WgXcQ".into(),
            url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".into(),
            title: "Canción {channel}".into(),
            channel: "Artista".into(),
            thumbnail: String::new(),
            duration_seconds: 1,
            view_count: 0,
        }
    }
    fn wave() -> Vec<u8> {
        let samples = 8000u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + samples * 2).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&8000u32.to_le_bytes());
        bytes.extend_from_slice(&16000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(samples * 2).to_le_bytes());
        for frame in 0..samples {
            bytes.extend_from_slice(
                &((frame as f64 * 0.3).sin().mul_add(2000.0, 0.0) as i16).to_le_bytes(),
            );
        }
        bytes
    }
    #[test]
    fn templates_are_literal_portable_and_reject_unknown_operators() {
        let item = media();
        assert_eq!(
            render_name(
                "{title} - {channel} - {video_id} - {quality}",
                &item,
                false,
                "192"
            )
            .unwrap(),
            "Canción {channel} - Artista - dQw4w9WgXcQ - 192.mp3"
        );
        assert_eq!(
            render_name("{{{media_type}}}", &item, true, "best").unwrap(),
            "{video}.mp4"
        );
        for template in [
            "{title.__class__}",
            "{title!s}",
            "{title:30}",
            "{unknown}",
            "{title",
            "title}",
        ] {
            assert!(
                render_name(template, &item, false, "192").is_err(),
                "{template}"
            );
        }
        assert!(!render_name("../{channel}/song", &item, false, "192")
            .unwrap()
            .contains('/'));
    }
    #[test]
    fn publication_recovers_after_crash_without_overwriting_originals() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let library = temp.path().join("library");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(&library).unwrap();
        let prepared = cache.join("ready.mp3");
        std::fs::write(&prepared, b"complete prepared content").unwrap();
        std::fs::write(library.join("song.mp3"), b"user original").unwrap();
        let token = CancellationToken::new();
        let first = publish_prepared(&prepared, &library, "song.mp3", &cache, &token).unwrap();
        assert_eq!(first.file_name().unwrap(), "song (1).mp3");
        assert_eq!(
            std::fs::read(library.join("song.mp3")).unwrap(),
            b"user original"
        );
        assert_eq!(
            publish_prepared(&prepared, &library, "song.mp3", &cache, &token).unwrap(),
            first
        );
        assert_eq!(std::fs::read_dir(&library).unwrap().count(), 2);
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(publish_prepared(&prepared, &library, "new.mp3", &cache, &cancelled).is_err());
        assert!(!library.join("new.mp3").exists());
    }
    #[test]
    fn retry_cache_survives_database_reopen_without_connection_credentials() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let prefs = Preferences::current(&Settings::default());
        let job = core
            .create_job(
                "download",
                &json!({"url":media().url,"preferences":prefs.value()}),
            )
            .unwrap();
        let original = cache_id(&job);
        let directory = cache_directory(profile.path(), &original).unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("piece"), b"committed").unwrap();
        drop(core);
        let reopened = AppCore::open(profile.path()).unwrap();
        let previous = reopened.job(&job.id).unwrap();
        assert_eq!(previous.status, JobStatus::Interrupted);
        let retried=reopened.create_job("download",&json!({"url":media().url,"cache_id":cache_id(&previous),"preferences":previous.payload["preferences"]})).unwrap();
        assert_eq!(cache_id(&retried), original);
        assert_eq!(
            std::fs::read(
                cache_directory(profile.path(), &cache_id(&retried))
                    .unwrap()
                    .join("piece")
            )
            .unwrap(),
            b"committed"
        );
        let serialized = prefs.value().to_string();
        for forbidden in ["cookies", "proxy", "stream_url", "Authorization"] {
            assert!(!serialized.contains(forbidden));
        }
        assert!(cache_directory(profile.path(), "../../outside").is_err());
    }
    #[tokio::test]
    async fn complete_cached_audio_is_converted_tagged_published_and_reindexed_without_network() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let item = media();
        let prefs = Preferences {
            thumbnail: false,
            subtitles: false,
            filename_template: "{channel} - {video_id}".into(),
            ..Preferences::current(&Settings::default())
        };
        let job = core
            .create_job("download", &json!({"url":item.url}))
            .unwrap();
        let directory = cache_directory(profile.path(), &job.id).unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        let source = wave();
        std::fs::write(directory.join("audio.m4a"), &source).unwrap();
        write_json(&directory.join("source.json"),&json!({"media":item,"video":false,"primary":ResumeKey{media_id:item.id.clone(),itag:140,audio_track_id:None,dynamic_range_compressed:false,total_bytes:source.len() as u64},"audio":null,"height":0})).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(32);
        let progress = Progress(tx);
        let youtube = YoutubeClient::new().unwrap();
        let downloader = Downloader::new().unwrap();
        let ready = run_download(
            &core,
            profile.path(),
            core.library_dir(),
            &job.id,
            &item,
            false,
            &prefs,
            &youtube,
            &downloader,
            0,
            &CancellationToken::new(),
            &progress,
        )
        .await
        .unwrap();
        assert!(ready.track.path.exists());
        assert_eq!(ready.track.title, item.title);
        assert_eq!(ready.track.artist, item.channel);
        assert_eq!(ready.track.filename, "Artista - dQw4w9WgXcQ.mp3");
        assert!(ready.warnings.is_empty());
        assert_eq!(
            core.media_metadata(&ready.track.id)
                .unwrap()
                .title
                .as_deref(),
            Some(item.title.as_str())
        );
        let mut decoded = yt_media::decode_file(&ready.track.path).unwrap();
        assert!(decoded.by_ref().count() > 0);
        assert!(decoded.decoding_error().is_none());
        // A crash after atomic publication but before job completion must reuse
        // the same complete file and index, not make a duplicate copy.
        let again = run_download(
            &core,
            profile.path(),
            core.library_dir(),
            &job.id,
            &item,
            false,
            &prefs,
            &youtube,
            &downloader,
            0,
            &CancellationToken::new(),
            &progress,
        )
        .await
        .unwrap();
        assert_eq!(again.track.id, ready.track.id);
        assert_eq!(core.library("").unwrap().len(), 1);
        assert_eq!(std::fs::read_dir(core.library_dir()).unwrap().count(), 1);
    }
    #[test]
    fn active_cache_cannot_be_discarded_and_inactive_cache_removes_only_its_files() {
        let profile = tempfile::tempdir().unwrap();
        let id = "11111111-2222-3333-4444-555555555555";
        let directory = cache_directory(profile.path(), id).unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("owner.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        std::fs::write(directory.join("source"), b"partial").unwrap();
        std::fs::write(profile.path().join("original.mp3"), b"original").unwrap();
        assert!(remove_cache(profile.path(), id, &profile.path().join("library")).is_err());
        assert!(directory.join("source").exists());
        drop(lock);
        let external_lock = lock_cache(&directory).unwrap();
        assert!(remove_cache(profile.path(), id, &profile.path().join("library")).is_err());
        assert!(directory.join("source").exists());
        drop(external_lock);
        remove_cache(profile.path(), id, &profile.path().join("library")).unwrap();
        assert!(!directory.exists());
        assert!(directory.with_extension("lock").is_file());
        assert_eq!(
            std::fs::read(profile.path().join("original.mp3")).unwrap(),
            b"original"
        );
    }
    #[tokio::test]
    #[ignore = "Requires public YouTube network access; creates only a temporary profile"]
    async fn real_video_download_pipeline() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let item = SearchResult {
            id: "aqz-KE-bpKQ".into(),
            url: "https://www.youtube.com/watch?v=aqz-KE-bpKQ".into(),
            title: "Blender probe".into(),
            channel: String::new(),
            thumbnail: String::new(),
            duration_seconds: 635,
            view_count: 0,
        };
        let prefs = Preferences {
            video_quality: "144".into(),
            thumbnail: true,
            subtitles: true,
            ..Preferences::current(&Settings::default())
        };
        let job = core
            .create_job(
                "download",
                &json!({"url":item.url,"preferences":prefs.value()}),
            )
            .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(32);
        let ready = run_download(
            &core,
            profile.path(),
            core.library_dir(),
            &job.id,
            &item,
            true,
            &prefs,
            &YoutubeClient::new().unwrap(),
            &Downloader::new().unwrap(),
            1,
            &CancellationToken::new(),
            &Progress(tx),
        )
        .await
        .unwrap();
        assert_eq!(ready.track.media_type, "video");
        assert_eq!(ready.height, 144);
        assert!(ready.track.size_bytes > 14_000_000);
        assert!(!ready.track.artist.is_empty());
        assert!(core.artwork_path(&ready.track.id).unwrap().is_some());
        assert_eq!(core.library("").unwrap().len(), 1);
        println!(
            "APP pipeline: {} bytes, {}p, artwork cached, {} warnings: {:?}",
            ready.track.size_bytes,
            ready.height,
            ready.warnings.len(),
            ready.warnings
        );
    }
    #[test]
    fn clearing_old_history_preserves_active_and_paused_retry_bytes() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let original = core
            .create_job("download", &json!({"url":media().url}))
            .unwrap();
        core.cancel_job(&original.id).unwrap();
        let directory = cache_directory(profile.path(), &original.id).unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("part"), b"committed").unwrap();
        let retry = core
            .create_job(
                "download",
                &json!({"url":media().url,"cache_id":original.id}),
            )
            .unwrap();
        remove_history_record(&core, profile.path(), &original.id).unwrap();
        assert!(core.job(&original.id).is_err());
        assert!(directory.join("part").exists());
        assert!(remove_history_record(&core, profile.path(), &retry.id).is_err());
        core.cancel_job(&retry.id).unwrap();
        remove_history_record(&core, profile.path(), &retry.id).unwrap();
        assert!(!directory.exists());
    }
    #[tokio::test]
    #[ignore = "Requires public YouTube network access; creates only a temporary profile"]
    async fn real_audio_subtitle_metadata_pipeline() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let item = SearchResult {
            id: "kjBOesZCoqc".into(),
            url: "https://www.youtube.com/watch?v=kjBOesZCoqc".into(),
            title: "Caption probe".into(),
            channel: String::new(),
            thumbnail: String::new(),
            duration_seconds: 0,
            view_count: 0,
        };
        let prefs = Preferences {
            thumbnail: true,
            subtitles: true,
            language: "en".into(),
            ..Preferences::current(&Settings::default())
        };
        let job = core
            .create_job(
                "download",
                &json!({"url":item.url,"preferences":prefs.value()}),
            )
            .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(32);
        let ready = run_download(
            &core,
            profile.path(),
            core.library_dir(),
            &job.id,
            &item,
            false,
            &prefs,
            &YoutubeClient::new().unwrap(),
            &Downloader::new().unwrap(),
            1,
            &CancellationToken::new(),
            &Progress(tx),
        )
        .await
        .unwrap();
        assert_eq!(ready.track.extension, "mp3");
        assert_eq!(ready.subtitle_files.len(), 1);
        assert!(ready.warnings.is_empty(), "{:?}", ready.warnings);
        assert!(std::fs::read_to_string(&ready.subtitle_files[0])
            .unwrap()
            .starts_with("WEBVTT"));
        let metadata = core.media_metadata(&ready.track.id).unwrap();
        assert!(metadata.artwork_path.is_some());
        assert!(!metadata.artist.unwrap().is_empty());
        let tracks = crate::local_media::subtitle_tracks(&core, &ready.track.id);
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].language, "en");
        assert!(tracks[0].default);
        println!(
            "APP audio pipeline: {} MP3 bytes, embedded artwork/title/artist, {} visible subtitle track",
            ready.track.size_bytes,
            tracks.len()
        );
    }
    #[test]
    fn restart_cleans_only_the_owned_interrupted_publication() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let library = temp.path().join("library");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(&library).unwrap();
        let abandoned = temp.path().join(".ytu-publishing-abandoned");
        std::fs::create_dir_all(&abandoned).unwrap();
        std::fs::write(abandoned.join("partial.mp4"), b"partial").unwrap();
        write_json(
            &abandoned.join(".ytu-owner.json"),
            &json!({"cache_id":"cache"}),
        )
        .unwrap();
        write_json(
            &cache.join("publication.json"),
            &json!({"filename":"song.mp4","staging":".ytu-publishing-abandoned"}),
        )
        .unwrap();
        let prepared = cache.join("ready.mp4");
        std::fs::write(&prepared, b"complete").unwrap();
        let published = publish_prepared(
            &prepared,
            &library,
            "song.mp4",
            &cache,
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(!abandoned.exists());
        assert_eq!(std::fs::read(published).unwrap(), b"complete");
        let unrelated = temp.path().join(".ytu-publishing-unrelated");
        std::fs::create_dir_all(&unrelated).unwrap();
        std::fs::write(unrelated.join("original"), b"original").unwrap();
        write_json(
            &unrelated.join(".ytu-owner.json"),
            &json!({"cache_id":"different"}),
        )
        .unwrap();
        assert!(cleanup_staging(
            &library,
            &cache,
            &json!({"staging":".ytu-publishing-unrelated"})
        )
        .is_err());
        assert!(unrelated.join("original").exists());
    }
}
