//! Analysis of the songs the DJ mixes. While DJ mode is on and something
//! plays, the playing song and the one after it are measured in `ytu-worker`
//! (beats, bars, meter, tempo, key, loudness and sections) and kept in the
//! analysis cache for the transition planner. Once both are ready, their
//! vocals are measured too, one song at a time, giving way to any new
//! analysis. Requests for any other song are cancelled. See
//! `docs/systems/Dj.md`.

use crate::state::{use_app_signal, AppState};
use dioxus::prelude::*;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant, UNIX_EPOCH},
};
use yt_core::{MediaOrigin, Track, TrackAnalysis};
use yt_media::CancellationToken;
use yt_worker::{
    protocol::DeferReason, ClientError, Device, WorkerClient, WorkerConfig, ANALYZER_VERSION,
    BEAT_MODEL_FILENAME, IDLE_EXIT, VOCAL_MODEL_FILENAME,
};

/// Wait before asking again for an analysis the power governor held back or
/// a restarting worker could not take.
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// Songs whose analysis state is kept in memory; the least recently wanted
/// finished ones are forgotten beyond it.
const REMEMBERED_SONGS: usize = 32;
/// sa3.cpp's generation program, beside the executable, and the folder of
/// Stable Audio 3's weights in the models resources (`docs/RUST_PACKAGING.md`).
const FUSION_PROGRAM: &str = "sa3-generate";
const FUSION_MODELS_FOLDER: &str = "stable-audio-3";
/// The ONNX Runtime library as the packages ship it (`xtask package`).
#[cfg(target_os = "macos")]
const ONNX_RUNTIME_LIBRARY: &str = "libonnxruntime.dylib";
#[cfg(windows)]
const ONNX_RUNTIME_LIBRARY: &str = "onnxruntime.dll";
#[cfg(not(any(target_os = "macos", windows)))]
const ONNX_RUNTIME_LIBRARY: &str = "libonnxruntime.so";

/// Where one song's analysis stands.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AnalysisStatus {
    Analyzing,
    /// Measured on `device`, or read from the cache when `device` is `None`.
    Ready {
        analysis: Arc<TrackAnalysis>,
        device: Option<Device>,
    },
    /// Held back by the power governor; asked again after [`RETRY_AFTER`].
    Deferred(DeferReason),
    /// This installation or device cannot analyze; the technical reason.
    Unavailable(String),
    /// This song could not be analyzed; the technical reason.
    Failed(String),
}

/// The playing song's analysis, for the developer view.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ShownAnalysis {
    pub(crate) track_id: String,
    pub(crate) status: AnalysisStatus,
}

/// The DJ's analysis state: one entry per recently wanted song.
#[derive(Clone)]
pub(crate) struct DjAnalysis {
    pub(crate) shown: Signal<Option<ShownAnalysis>>,
    songs: Rc<RefCell<HashMap<String, Song>>>,
    client: Rc<RefCell<Option<Option<Arc<WorkerClient>>>>>,
    reported_unavailable: Rc<Cell<bool>>,
}

struct Song {
    status: AnalysisStatus,
    running: Option<CancellationToken>,
    retry_at: Option<Instant>,
    wanted_at: Instant,
    /// The content key the analysis is stored under, to add its vocals.
    key: Option<String>,
    vocals: Vocals,
}

/// Where the measurement of one song's vocals stands. It runs once the
/// analyses the DJ needs are ready, gives way to any new analysis, and is
/// tried once per session.
#[derive(Clone, Debug, Default)]
enum Vocals {
    #[default]
    Wanted,
    Measuring(CancellationToken),
    Settled,
}

/// The audio the worker reads for one song.
#[derive(Clone)]
struct Source {
    origin: MediaOrigin,
    path: PathBuf,
    /// Size of a complete online stream; library files are keyed by their
    /// size and modification time instead.
    stream_bytes: Option<u64>,
}

enum Finish {
    Done(AnalysisStatus),
    Retry(AnalysisStatus),
    Cancelled,
}

pub(crate) fn use_dj_analysis() -> DjAnalysis {
    DjAnalysis {
        shown: use_app_signal(|| None),
        songs: use_hook(Rc::default),
        client: use_hook(Rc::default),
        reported_unavailable: use_hook(Rc::default),
    }
}

impl DjAnalysis {
    /// The analysis of `track_id`, once it is ready.
    pub(crate) fn ready(&self, track_id: &str) -> Option<Arc<TrackAnalysis>> {
        match &self.songs.borrow().get(track_id)?.status {
            AnalysisStatus::Ready { analysis, .. } => Some(analysis.clone()),
            _ => None,
        }
    }

    /// Record a finished analysis of `track_id`, as a worker reply would.
    #[cfg(test)]
    pub(crate) fn insert_ready(&self, track_id: &str, analysis: TrackAnalysis) {
        self.songs.borrow_mut().insert(
            track_id.to_owned(),
            Song {
                status: AnalysisStatus::Ready {
                    analysis: Arc::new(analysis),
                    device: None,
                },
                running: None,
                retry_at: None,
                wanted_at: Instant::now(),
                key: None,
                vocals: Vocals::Settled,
            },
        );
    }

    /// Stop every vocal measurement so a new analysis or a fusion does not
    /// wait for it; each starts again later.
    pub(crate) fn preempt_vocals(&self) {
        for song in self.songs.borrow_mut().values_mut() {
            if let Vocals::Measuring(cancel) = &song.vocals {
                cancel.cancel();
                song.vocals = Vocals::Wanted;
            }
        }
    }

    /// Whether an analysis of one of `ids` is still running.
    fn analyzing(&self, ids: &[&str]) -> bool {
        let songs = self.songs.borrow();
        ids.iter().any(|id| {
            songs
                .get(*id)
                .is_some_and(|song| matches!(song.status, AnalysisStatus::Analyzing))
        })
    }

    /// Cancel the analysis of every song not in `keep`.
    fn cancel_except(&self, keep: &[&str]) {
        let mut songs = self.songs.borrow_mut();
        for (id, song) in songs.iter_mut() {
            if keep.contains(&id.as_str()) {
                continue;
            }
            if let Some(cancel) = song.running.take() {
                cancel.cancel();
            }
            if let Vocals::Measuring(cancel) = &song.vocals {
                cancel.cancel();
                song.vocals = Vocals::Wanted;
            }
        }
        songs.retain(|_, song| {
            !matches!(song.status, AnalysisStatus::Analyzing) || song.running.is_some()
        });
        if songs.len() > REMEMBERED_SONGS {
            let mut finished: Vec<(Instant, String)> = songs
                .iter()
                .filter(|(id, song)| song.running.is_none() && !keep.contains(&id.as_str()))
                .map(|(id, song)| (song.wanted_at, id.clone()))
                .collect();
            finished.sort();
            let excess = songs.len() - REMEMBERED_SONGS;
            for (_, id) in finished.into_iter().take(excess) {
                songs.remove(&id);
            }
        }
    }

    /// The worker client, built on first use; `None` when this installation
    /// lacks the worker, ONNX Runtime or the beat model.
    pub(crate) fn client(&self, profile: &Path) -> Option<Arc<WorkerClient>> {
        self.client
            .borrow_mut()
            .get_or_insert_with(|| {
                worker_config(profile).map(|config| Arc::new(WorkerClient::new(config)))
            })
            .clone()
    }
}

fn worker_config(profile: &Path) -> Option<WorkerConfig> {
    Some(WorkerConfig {
        executable: crate::bundled::program("YTU_WORKER", "ytu-worker")?,
        runtime_library: crate::bundled::library("YTU_ONNXRUNTIME", ONNX_RUNTIME_LIBRARY)?,
        beat_model: crate::bundled::model(
            "YTU_BEAT_MODEL",
            BEAT_MODEL_FILENAME,
            "../yt-mir/models",
        )?,
        vocal_model: crate::bundled::model(
            "YTU_VOCAL_MODEL",
            VOCAL_MODEL_FILENAME,
            "../yt-mir/models",
        ),
        // Without its program and weights, fusions are not planned.
        fusion: crate::bundled::program("YTU_SA3_PROGRAM", FUSION_PROGRAM)
            .zip(crate::bundled::models_folder(
                "YTU_SA3_MODELS",
                FUSION_MODELS_FOLDER,
            ))
            .and_then(|(program, models)| yt_worker::fusion::Generator::find(program, models)),
        model_cache: profile.join("dj").join("compiled"),
        idle_exit: IDLE_EXIT,
    })
}

impl AppState {
    /// Ask for the analyses the DJ needs now and cancel the rest. Called from
    /// the shared poll while DJ mode is on; does nothing while paused.
    pub(crate) fn request_dj_analyses(&self) {
        if !*self.playing.peek() {
            return;
        }
        let current = self.current.peek().clone();
        let next = self.upcoming_track();
        let keep: Vec<&str> = [current.as_ref(), next.as_ref()]
            .into_iter()
            .flatten()
            .map(|track| track.id.as_str())
            .collect();
        self.dj_analysis.cancel_except(&keep);
        if let Some(track) = &current {
            if let Some(source) = self.analysis_source(track, true) {
                self.request_analysis(track, source);
            }
        }
        if let Some(track) = &next {
            if let Some(source) = self.analysis_source(track, false) {
                self.request_analysis(track, source);
            }
        }
        if !self.dj_analysis.analyzing(&keep) && !self.preparing_fusion() {
            let songs = [(current.as_ref(), true), (next.as_ref(), false)];
            for (track, playing) in songs {
                if track.is_some_and(|track| self.request_vocals(track, playing)) {
                    break;
                }
            }
        }
        self.show_dj_analysis(current.as_ref());
    }

    /// Stop every analysis: DJ mode was turned off.
    pub(crate) fn cancel_dj_analyses(&self) {
        self.dj_analysis.cancel_except(&[]);
    }

    /// Forget everything measured in this session; the listening history was
    /// erased and the cache rows of online media with it.
    pub(crate) fn forget_dj_analyses(&self) {
        self.cancel_dj_analyses();
        self.dj_analysis.songs.borrow_mut().clear();
        let mut shown = self.dj_analysis.shown;
        shown.set(None);
    }

    /// The file and origin of `track`'s audio when it can be read now, as
    /// [`Self::analysis_source`] finds it: for the vocal stems of a mix.
    pub(crate) fn audio_file(
        &self,
        track: &Track,
        playing: bool,
    ) -> Option<(MediaOrigin, PathBuf)> {
        self.analysis_source(track, playing)
            .map(|source| (source.origin, source.path))
    }

    /// The audio to analyze for `track`, when it can be read now. Online
    /// media is analyzed from its own download once it is complete (its
    /// playback's, or the stream the DJ prepared for it), so it is never
    /// downloaded twice.
    fn analysis_source(&self, track: &Track, playing: bool) -> Option<Source> {
        if track.media_type == "video" {
            return None;
        }
        if crate::remote::is_remote(track) {
            let (path, bytes) = if playing {
                let session = self.stream_session.peek();
                let session = session.as_ref()?;
                (session.complete_file()?, session.snapshot().total_bytes)
            } else {
                self.prepared_stream_file(&track.id)?
            };
            return Some(Source {
                origin: MediaOrigin::YouTube,
                path,
                stream_bytes: Some(bytes),
            });
        }
        (!track.missing && !track.path.as_os_str().is_empty()).then(|| Source {
            origin: MediaOrigin::Library,
            path: track.path.clone(),
            stream_bytes: None,
        })
    }

    fn request_analysis(&self, track: &Track, source: Source) {
        let now = Instant::now();
        {
            let mut songs = self.dj_analysis.songs.borrow_mut();
            if let Some(song) = songs.get_mut(&track.id) {
                song.wanted_at = now;
                if song.running.is_some() || song.retry_at.is_none_or(|at| now < at) {
                    return;
                }
            }
        }
        self.dj_analysis.preempt_vocals();
        let cancel = CancellationToken::new();
        self.dj_analysis.songs.borrow_mut().insert(
            track.id.clone(),
            Song {
                status: AnalysisStatus::Analyzing,
                running: Some(cancel.clone()),
                retry_at: None,
                wanted_at: now,
                key: None,
                vocals: Vocals::Wanted,
            },
        );
        let state = self.clone();
        let id = track.id.clone();
        dioxus::core::spawn_forever(async move {
            let (finish, key) = state.measure(&id, source, &cancel).await;
            if cancel.is_cancelled() {
                return;
            }
            let (status, retry_at) = match finish {
                Finish::Done(status) => (status, None),
                Finish::Retry(status) => (status, Some(Instant::now() + RETRY_AFTER)),
                Finish::Cancelled => return,
            };
            if let Some(song) = state.dj_analysis.songs.borrow_mut().get_mut(&id) {
                song.status = status;
                song.running = None;
                song.retry_at = retry_at;
                song.key = key;
            }
            let current = state.current.peek().clone();
            if current.as_ref().is_some_and(|track| track.id == id) {
                state.show_dj_analysis(current.as_ref());
            }
        });
    }

    /// Measure `source`, or read it from the cache, and keep the result; with
    /// the content key it is stored under.
    async fn measure(
        &self,
        id: &str,
        source: Source,
        cancel: &CancellationToken,
    ) -> (Finish, Option<String>) {
        let core = self.services.core.clone();
        let lookup_id = id.to_owned();
        let lookup = tokio::task::spawn_blocking(move || {
            let key = content_key(&source)?;
            let cached = core
                .track_analysis(&lookup_id, &key, ANALYZER_VERSION)
                .ok()
                .flatten();
            Some((key, cached, source))
        })
        .await;
        let Ok(Some((key, cached, source))) = lookup else {
            let failed = AnalysisStatus::Failed("the audio file cannot be read".into());
            return (Finish::Done(failed), None);
        };
        if let Some(analysis) = cached {
            let ready = AnalysisStatus::Ready {
                analysis: Arc::new(analysis),
                device: None,
            };
            return (Finish::Done(ready), Some(key));
        }
        let Some(client) = self.dj_analysis.client(&self.services.profile) else {
            let reason = "this installation does not include the DJ analysis".into();
            return (self.unavailable(reason), None);
        };
        let finish = match client.analyze(source.origin, source.path, cancel).await {
            Ok(analyzed) => {
                let analysis = Arc::new(analyzed.analysis);
                self.store_analysis(id, &key, analysis.clone()).await;
                Finish::Done(AnalysisStatus::Ready {
                    analysis,
                    device: Some(analyzed.device),
                })
            }
            Err(ClientError::Cancelled) => Finish::Cancelled,
            Err(ClientError::Deferred(reason)) => Finish::Retry(AnalysisStatus::Deferred(reason)),
            Err(error @ (ClientError::Restarting(_) | ClientError::Crashed)) => {
                crate::native::record_log(
                    &self.services.profile,
                    &format!("The DJ worker did not finish an analysis: {error}"),
                );
                Finish::Retry(AnalysisStatus::Failed(error.to_string()))
            }
            Err(ClientError::Unavailable(reason)) => self.unavailable(reason),
            Err(error) => Finish::Done(AnalysisStatus::Failed(error.to_string())),
        };
        (finish, Some(key))
    }

    /// Keep `analysis` of `id` in the cache under `key`; a failure is logged.
    async fn store_analysis(&self, id: &str, key: &str, analysis: Arc<TrackAnalysis>) {
        let core = self.services.core.clone();
        let (id, key) = (id.to_owned(), key.to_owned());
        let saved =
            tokio::task::spawn_blocking(move || core.store_track_analysis(&id, &key, &analysis))
                .await;
        if let Ok(Err(error)) = saved {
            crate::native::record_log(
                &self.services.profile,
                &format!("A DJ analysis could not be saved: {error}"),
            );
        }
    }

    /// Measure the vocals of `track` (`playing` when it is the current song)
    /// once its analysis is ready, when this installation can. `true` while a
    /// measurement runs, so only one does at a time.
    fn request_vocals(&self, track: &Track, playing: bool) -> bool {
        let (analysis, key) = {
            let songs = self.dj_analysis.songs.borrow();
            let Some(song) = songs.get(&track.id) else {
                return false;
            };
            match (&song.vocals, &song.status, &song.key) {
                (Vocals::Measuring(_), ..) => return true,
                (Vocals::Wanted, AnalysisStatus::Ready { analysis, .. }, Some(key))
                    if analysis.vocal_db.is_empty() =>
                {
                    (analysis.clone(), key.clone())
                }
                _ => return false,
            }
        };
        let client = self.dj_analysis.client(&self.services.profile);
        let (Some(client), Some(source)) = (
            client.filter(|client| client.measures_vocals()),
            self.analysis_source(track, playing),
        ) else {
            return false;
        };
        let cancel = CancellationToken::new();
        if let Some(song) = self.dj_analysis.songs.borrow_mut().get_mut(&track.id) {
            song.vocals = Vocals::Measuring(cancel.clone());
        }
        let state = self.clone();
        let id = track.id.clone();
        dioxus::core::spawn_forever(async move {
            let measured = client
                .measure_vocals(source.origin, source.path, &cancel)
                .await;
            if cancel.is_cancelled() {
                return;
            }
            let with_vocals = measured.ok().and_then(|vocals| {
                let mut analysis = (*analysis).clone();
                analysis.vocal_db = vocals.vocal_db;
                analysis.validate().ok().map(|()| Arc::new(analysis))
            });
            if let Some(analysis) = &with_vocals {
                state.store_analysis(&id, &key, analysis.clone()).await;
            }
            if let Some(song) = state.dj_analysis.songs.borrow_mut().get_mut(&id) {
                song.vocals = Vocals::Settled;
                if let (Some(with_vocals), AnalysisStatus::Ready { analysis, .. }) =
                    (with_vocals, &mut song.status)
                {
                    *analysis = with_vocals;
                }
            }
            let current = state.current.peek().clone();
            if current.as_ref().is_some_and(|track| track.id == id) {
                state.show_dj_analysis(current.as_ref());
            }
        });
        true
    }

    /// Analysis is impossible here; logged once per session.
    fn unavailable(&self, reason: String) -> Finish {
        if !self.dj_analysis.reported_unavailable.replace(true) {
            crate::native::record_log(
                &self.services.profile,
                &format!("The DJ analysis is unavailable: {reason}"),
            );
        }
        Finish::Done(AnalysisStatus::Unavailable(reason))
    }

    /// Show the state of `current`'s analysis in the developer view.
    fn show_dj_analysis(&self, current: Option<&Track>) {
        let shown = current.and_then(|track| {
            let songs = self.dj_analysis.songs.borrow();
            let song = songs.get(&track.id)?;
            Some(ShownAnalysis {
                track_id: track.id.clone(),
                status: song.status.clone(),
            })
        });
        let same = {
            let previous = self.dj_analysis.shown.peek();
            match (previous.as_ref(), shown.as_ref()) {
                (Some(previous), Some(shown)) => {
                    previous.track_id == shown.track_id
                        && same_status(&previous.status, &shown.status)
                }
                (None, None) => true,
                _ => false,
            }
        };
        if !same {
            let mut signal = self.dj_analysis.shown;
            signal.set(shown);
        }
    }
}

/// Equal statuses, comparing analyses by identity: the poll calls this every
/// tick and a deep comparison of thousands of beats would be wasted.
fn same_status(a: &AnalysisStatus, b: &AnalysisStatus) -> bool {
    match (a, b) {
        (
            AnalysisStatus::Ready {
                analysis: left,
                device: left_device,
            },
            AnalysisStatus::Ready {
                analysis: right,
                device: right_device,
            },
        ) => Arc::ptr_eq(left, right) && left_device == right_device,
        (AnalysisStatus::Ready { .. }, _) | (_, AnalysisStatus::Ready { .. }) => false,
        _ => a == b,
    }
}

/// What tells two encodings of the same media apart: the size of a complete
/// stream, or a library file's size and modification time.
fn content_key(source: &Source) -> Option<String> {
    if let Some(bytes) = source.stream_bytes {
        return Some(format!("stream:{bytes}"));
    }
    let metadata = std::fs::metadata(&source.path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!("file:{}:{modified}", metadata.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{remote_track, TestApp};

    fn library_track(path: &Path) -> Track {
        Track {
            media_type: "audio".into(),
            path: path.to_owned(),
            ..remote_track("aqz-KE-bpKQ", "song")
        }
    }

    #[test]
    fn library_files_are_keyed_by_size_and_modification_time() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("song.flac");
        std::fs::write(&path, b"12345").unwrap();
        let source = Source {
            origin: MediaOrigin::Library,
            path: path.clone(),
            stream_bytes: None,
        };
        let key = content_key(&source).unwrap();
        assert!(key.starts_with("file:5:"), "{key}");
        assert!(key.len() <= yt_core::MAX_ANALYSIS_KEY_BYTES);
        let stream = Source {
            stream_bytes: Some(3_456_789),
            ..source
        };
        assert_eq!(content_key(&stream).unwrap(), "stream:3456789");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            content_key(&Source {
                origin: MediaOrigin::Library,
                path,
                stream_bytes: None,
            }),
            None
        );
    }

    #[tokio::test]
    async fn only_audio_that_can_be_read_now_is_analyzed() {
        let folder = tempfile::tempdir().unwrap();
        let mut app = TestApp::new();
        let path = folder.path().join("song.flac");
        app.run(|state| {
            let library = library_track(&path);
            let source = state.analysis_source(&library, false).unwrap();
            assert_eq!(source.origin, MediaOrigin::Library);
            let mut missing = library.clone();
            missing.missing = true;
            assert!(state.analysis_source(&missing, true).is_none());
            let mut video = library.clone();
            video.media_type = "video".into();
            assert!(state.analysis_source(&video, true).is_none());
            let online = remote_track("abcdefghijk", "Online");
            assert!(
                state.analysis_source(&online, true).is_none(),
                "online media waits for its own complete download"
            );
            assert!(
                state.analysis_source(&online, false).is_none(),
                "a next online song waits for its prepared stream"
            );
        });
    }

    #[tokio::test]
    async fn requests_follow_the_songs_the_dj_needs_and_stop_with_dj_mode() {
        let folder = tempfile::tempdir().unwrap();
        let mut app = TestApp::new();
        let current = library_track(&folder.path().join("a.flac"));
        let next = Track {
            id: "next".into(),
            ..library_track(&folder.path().join("b.flac"))
        };
        app.run(|state| {
            state.current.set(Some(current.clone()));
            state.queue.set(vec![next.clone()]);
            state.playing.set(true);
            state.request_dj_analyses();
            let running = |id: &str| {
                state
                    .dj_analysis
                    .songs
                    .borrow()
                    .get(id)
                    .is_some_and(|song| song.running.is_some())
            };
            assert!(running(&current.id) && running(&next.id));
            state.queue.set(Vec::new());
            state.request_dj_analyses();
            assert!(running(&current.id));
            assert!(
                !running(&next.id),
                "a song that is no longer next is cancelled"
            );
            state.cancel_dj_analyses();
            assert!(!running(&current.id));
        });
    }

    #[tokio::test]
    async fn a_new_analysis_stops_vocal_measurements_until_it_is_done() {
        let mut app = TestApp::new();
        app.run(|state| {
            state.dj_analysis.insert_ready("song", empty_analysis());
            let cancel = CancellationToken::new();
            if let Some(song) = state.dj_analysis.songs.borrow_mut().get_mut("song") {
                song.vocals = Vocals::Measuring(cancel.clone());
            }
            assert!(!state.dj_analysis.analyzing(&["song"]));
            state.dj_analysis.preempt_vocals();
            assert!(cancel.is_cancelled());
            assert!(matches!(
                state.dj_analysis.songs.borrow()["song"].vocals,
                Vocals::Wanted
            ));
        });
    }

    fn empty_analysis() -> TrackAnalysis {
        TrackAnalysis {
            analyzer_version: ANALYZER_VERSION,
            sample_rate: 44_100,
            duration_frames: 44_100 * 6,
            beats: Vec::new(),
            downbeats: Vec::new(),
            meter: None,
            tempo_bpm: None,
            tempo_stability: 0.0,
            key: None,
            integrated_lufs: None,
            true_peak_dbtp: -1.0,
            short_term_lufs: Vec::new(),
            bar_energy: Vec::new(),
            sections: Vec::new(),
            vocal_db: Vec::new(),
            momentary_lufs: Vec::new(),
        }
    }

    #[test]
    fn statuses_compare_analyses_by_identity() {
        let analysis = Arc::new(empty_analysis());
        let ready = |analysis: &Arc<TrackAnalysis>| AnalysisStatus::Ready {
            analysis: analysis.clone(),
            device: None,
        };
        assert!(same_status(&ready(&analysis), &ready(&analysis)));
        let copy = Arc::new((*analysis).clone());
        assert!(!same_status(&ready(&analysis), &ready(&copy)));
        assert!(same_status(
            &AnalysisStatus::Analyzing,
            &AnalysisStatus::Analyzing
        ));
        assert!(!same_status(&AnalysisStatus::Analyzing, &ready(&analysis)));
    }
}
