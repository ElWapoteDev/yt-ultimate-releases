//! Small DOM adapter for the system WebView media element. Playback state,
//! validation, persistence and queue decisions remain in Rust.
use crate::state::AppState;
use dioxus::prelude::*;
use serde_json::{json, Value};

/// Keep the library identity while preparing containers the system player cannot
/// open directly. Cancellation uses the same generation as remote preparation,
/// so a late completion can never replace a newer selection.
pub fn start_local(mut state: AppState, track: yt_core::Track, position: f64) {
    state.cancel_remote();
    if let Ok(audio) = &state.services.audio {
        if let Err(error) = audio.stop() {
            state.notify_error(error.to_string());
            return;
        }
    }
    state.record_session_departure(&track);
    state.current.set(Some(track.clone()));
    state.video_generation += 1;
    state.video_track.set(None);
    state.video_url.set(None);
    state.video_open.set(true);
    state.playback_error.set(None);
    state.playing.set(false);
    state.position.set(if position.is_finite() {
        position.max(0.0)
    } else {
        0.0
    });
    state.duration.set(track.duration);
    state.persist_playback();
    if !track.extension.eq_ignore_ascii_case("mkv") {
        finish_local(&mut state, track, None);
        return;
    }
    let cancel = yt_media::CancellationToken::new();
    state.stream_request.set(Some(cancel.clone()));
    state.stream_loading.set(true);
    state.buffering.set(true);
    let generation = *state.stream_generation.peek();
    let preparing = state
        .begin_task("video", format!("Preparando «{}»", track.title))
        .stop(crate::activity::Stop::Playback);
    dioxus::core::spawn_forever(async move {
        let input = track.path.clone();
        let worker_cancel = cancel.clone();
        let result = tokio::task::spawn_blocking(move || {
            yt_media::prepare_local_video(&input, worker_cancel, |_| {})
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| result.map_err(|error| error.to_string()));
        drop(preparing);
        if cancel.is_cancelled() || *state.stream_generation.peek() != generation {
            return;
        }
        state.stream_loading.set(false);
        state.buffering.set(false);
        state.stream_request.set(None);
        match result {
            Ok(video) => {
                if video.duration_seconds().is_finite() && video.duration_seconds() > 0.0 {
                    state.duration.set(video.duration_seconds());
                }
                let warnings = video.warnings().join(" ");
                let source =
                    crate::local_media::register_prepared_local_video(std::sync::Arc::new(video));
                finish_local(&mut state, track, Some(source));
                if !warnings.is_empty() {
                    state.notify_warning(warnings);
                }
            }
            Err(error) => {
                state.playing.set(false);
                state.playback_error.set(Some(error.clone()));
                state.notify_error(format!("No se pudo abrir el vídeo: {error}"));
            }
        }
    });
}

fn finish_local(state: &mut AppState, track: yt_core::Track, source: Option<String>) {
    state.video_url.set(source);
    state.video_track.set(Some(track.clone()));
    state.playing.set(true);
    state
        .history
        .write()
        .retain(|previous| previous.id != track.id);
    state.history.write().insert(0, track.clone());
    state.playback_started(&track);
    crate::native::record_recent(&track.path);
    state.persist_playback();
}

const MEDIA_ADAPTER: &str = r#"
const q = await dioxus.recv();
const v = document.getElementById('ytu-video');
if (!v || v.dataset.mediaKey !== q.key) return null;
try {
    switch (q.op) {
        case 'init':
            v.volume = q.volume; v.playbackRate = q.rate;
            v.preservesPitch = true;
            if (Number.isFinite(q.position)) v.currentTime = Math.max(0, Math.min(q.position, Number.isFinite(v.duration) ? v.duration : q.position));
            v.dataset.ready = 'true';
            if (q.play) await v.play(); else v.pause();
            break;
        case 'play': await v.play(); break;
        case 'pause': v.pause(); break;
        case 'seek': v.currentTime = q.value; break;
        case 'volume': v.muted = false; v.volume = q.value; break;
        case 'rate': v.playbackRate = q.value; v.preservesPitch = true; break;
        case 'pip':
            if (document.pictureInPictureElement === v) await document.exitPictureInPicture();
            else if (document.pictureInPictureEnabled && v.requestPictureInPicture) await v.requestPictureInPicture();
            else if (v.webkitSupportsPresentationMode && v.webkitSupportsPresentationMode('picture-in-picture'))
                v.webkitSetPresentationMode(v.webkitPresentationMode === 'picture-in-picture' ? 'inline' : 'picture-in-picture');
            else return {error: 'El sistema no permite imagen en imagen para este vídeo.'};
            break;
        case 'fullscreen':
            if (document.fullscreenElement) await document.exitFullscreen();
            else if (v.requestFullscreen) await v.requestFullscreen();
            else if (v.webkitEnterFullscreen) v.webkitEnterFullscreen();
            else return {error: 'La pantalla completa no está disponible.'};
            break;
    }
    if (v.dataset.ready !== 'true') return null;
    return {position: v.currentTime, duration: Number.isFinite(v.duration) ? v.duration : 0,
            playing: !v.paused && !v.ended, volume: v.muted ? 0 : v.volume, rate: v.playbackRate};
} catch (e) { return {error: String(e.message || e)}; }
"#;

pub async fn invoke(key: String, mut action: Value) -> Result<Option<Value>, String> {
    action["key"] = json!(key);
    let eval = document::eval(MEDIA_ADAPTER);
    eval.send(action).map_err(|error| error.to_string())?;
    let result = eval.await.map_err(|error| error.to_string())?;
    if let Some(error) = result["error"].as_str() {
        return Err(error.to_owned());
    }
    Ok((!result.is_null()).then_some(result))
}

pub fn key(state: &AppState) -> Option<String> {
    state
        .video_track
        .peek()
        .as_ref()
        .map(|track| format!("{}:{}", track.id, *state.video_generation.peek()))
}

pub fn command(state: AppState, action: Value) {
    let Some(media_key) = key(&state) else {
        return;
    };
    dioxus::core::spawn_forever(async move {
        match invoke(media_key.clone(), action).await {
            Ok(Some(snapshot)) if key(&state).as_ref() == Some(&media_key) => {
                apply_snapshot(&state, snapshot)
            }
            Err(error) if key(&state).as_ref() == Some(&media_key) => state.notify_error(error),
            _ => {}
        }
    });
}

pub fn apply_snapshot(state: &AppState, snapshot: Value) {
    let mut state = state.clone();
    if let Some(position) = snapshot["position"]
        .as_f64()
        .filter(|v| v.is_finite() && *v >= 0.0)
    {
        state.position.set(position);
    }
    if let Some(duration) = snapshot["duration"]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.0)
    {
        state.duration.set(duration);
    }
    if let Some(playing) = snapshot["playing"].as_bool() {
        state.playing.set(playing);
    }
    if let Some(volume) = snapshot["volume"]
        .as_f64()
        .filter(|v| (0.0..=1.0).contains(v))
    {
        state.volume.set(volume as f32);
        if volume > 0.0 {
            state.previous_volume.set(volume as f32);
        }
    }
    if let Some(rate) = snapshot["rate"]
        .as_f64()
        .filter(|v| (0.25..=4.0).contains(v))
    {
        state.rate.set(rate as f32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{use_app_state, Services};
    use std::{cell::RefCell, rc::Rc, sync::mpsc, time::Duration};

    type Capture = Rc<RefCell<Option<AppState>>>;

    fn harness() -> Element {
        let services = use_context::<Rc<Services>>();
        let capture = use_context::<Capture>();
        *capture.borrow_mut() = Some(use_app_state(services));
        rsx! { div {} }
    }

    fn setup(services: Rc<Services>) -> (VirtualDom, AppState) {
        let capture: Capture = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(harness);
        dom.insert_any_root_context(Box::new(services));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let state = capture.borrow().as_ref().unwrap().clone();
        (dom, state)
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap()
    }

    // A bounded Matroska container with one opaque VP9 packet. These tests
    // exercise the real preparation/state pipeline, not the system decoder.
    fn mkv_fixture() -> Vec<u8> {
        fn element(id: u32, payload: impl AsRef<[u8]>) -> Vec<u8> {
            let id = id.to_be_bytes();
            let start = id.iter().position(|byte| *byte != 0).unwrap();
            let payload = payload.as_ref();
            let width = (1..=8)
                .find(|width| (payload.len() as u64) < (1u64 << (width * 7)) - 1)
                .unwrap();
            let size = ((payload.len() as u64) | (1u64 << (width * 7))).to_be_bytes();
            [&id[start..], &size[8 - width..], payload].concat()
        }
        let header = element(
            0x1a45dfa3,
            [
                element(0x4282, b"matroska"),
                element(0x4287, [4]),
                element(0x4285, [2]),
            ]
            .concat(),
        );
        let info = element(
            0x1549a966,
            [
                element(0x2ad7b1, 1_000_000u64.to_be_bytes()),
                element(0x4489, 12_000f64.to_be_bytes()),
                element(0x4d80, b"fixture"),
                element(0x5741, b"fixture"),
            ]
            .concat(),
        );
        let tracks = element(
            0x1654ae6b,
            element(
                0xae,
                [
                    element(0xd7, [1]),
                    element(0x73c5, [1]),
                    element(0x83, [1]),
                    element(0x86, b"V_VP9"),
                    element(0xe0, [element(0xb0, [64]), element(0xba, [36])].concat()),
                ]
                .concat(),
            ),
        );
        let cluster = element(
            0x1f43b675,
            [
                element(0xe7, [0]),
                element(0xa3, [0x81, 0, 0, 0x80, 0x82, 0, 0]),
            ]
            .concat(),
        );
        [
            header,
            element(0x18538067, [info, tracks, cluster].concat()),
        ]
        .concat()
    }

    async fn block_worker_pool() -> (mpsc::Sender<()>, tokio::task::JoinHandle<()>) {
        let (release, wait) = mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let _ = started.send(());
            let _ = wait.recv();
        });
        ready.await.unwrap();
        (release, worker)
    }

    async fn drain_worker_pool(gate: (mpsc::Sender<()>, tokio::task::JoinHandle<()>)) {
        // The loader was queued behind the gate on this one-thread pool. The
        // barrier completes only after its result is ready for the DOM task.
        let barrier = tokio::task::spawn_blocking(|| {});
        drop(gate.0);
        gate.1.await.unwrap();
        barrier.await.unwrap();
    }

    #[test]
    fn cancelling_local_preparation_cannot_start_video_or_publish_a_late_error() {
        runtime().block_on(async {
            let profile = tempfile::tempdir().unwrap();
            let services = Rc::new(Services::for_test(profile.path()));
            let bytes = mkv_fixture();
            let track = services.core.import_bytes("cancel.mkv", &bytes).unwrap();
            let (mut dom, state) = setup(services.clone());
            let gate = block_worker_pool().await;
            dom.in_scope(ScopeId::ROOT, || {
                start_local(state.clone(), track.clone(), 4.5)
            });
            let token = state.stream_request.peek().clone().unwrap();
            dom.render_immediate_to_vec();
            dom.in_scope(ScopeId::ROOT, || state.toggle_play());
            assert!(token.is_cancelled());
            drain_worker_pool(gate).await;
            dom.render_immediate_to_vec();
            assert!(!*state.stream_loading.peek());
            assert!(!*state.buffering.peek());
            assert!(!*state.playing.peek());
            assert!(state.video_track.peek().is_none());
            assert!(state.video_url.peek().is_none());
            assert!(state.playback_error.peek().is_none());
            assert!(services.core.recently_played(10).unwrap().is_empty());
            assert_eq!(services.core.load_playback().unwrap().position_seconds, 4.5);
            assert_eq!(std::fs::read(&track.path).unwrap(), bytes);
        });
    }

    #[test]
    fn completed_old_local_preparation_cannot_replace_a_new_selection() {
        runtime().block_on(async {
            let profile = tempfile::tempdir().unwrap();
            let services = Rc::new(Services::for_test(profile.path()));
            let first = services
                .core
                .import_bytes("first.mkv", &mkv_fixture())
                .unwrap();
            let second = services
                .core
                .import_bytes("second.mp4", &[1, 2, 3])
                .unwrap();
            let (mut dom, state) = setup(services.clone());
            let gate = block_worker_pool().await;
            dom.in_scope(ScopeId::ROOT, || state.play(first.clone()));
            let old_token = state.stream_request.peek().clone().unwrap();
            dom.render_immediate_to_vec();
            drain_worker_pool(gate).await;
            // The worker completed successfully, but its DOM continuation has
            // not run yet. Replace it before delivering that old completion.
            dom.in_scope(ScopeId::ROOT, || state.play(second.clone()));
            assert!(old_token.is_cancelled());
            dom.render_immediate_to_vec();
            assert_eq!(state.current.peek().as_ref().unwrap().id, second.id);
            assert_eq!(state.video_track.peek().as_ref().unwrap().id, second.id);
            assert!(state.video_url.peek().is_none());
            assert!(*state.playing.peek());
            assert!(!*state.stream_loading.peek());
            assert!(state.playback_error.peek().is_none());
            assert_eq!(
                services.core.load_playback().unwrap().queue,
                vec![second.id.clone()]
            );
            let history = services.core.recently_played(10).unwrap();
            assert_eq!(history.len(), 1);
            assert_eq!(history[0].id(), second.id);
        });
    }

    #[tokio::test]
    async fn failed_local_preparation_retries_same_identity_and_saved_position() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let track = services
            .core
            .import_bytes("retry.mkv", b"invalid EBML")
            .unwrap();
        let (mut dom, state) = setup(services.clone());
        dom.in_scope(ScopeId::ROOT, || {
            start_local(state.clone(), track.clone(), 7.5)
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while state.playback_error.peek().is_none() {
                dom.render_immediate_to_vec();
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(!*state.stream_loading.peek());
        assert!(!*state.buffering.peek());
        assert!(!*state.playing.peek());
        assert!(state.stream_request.peek().is_none());
        assert!(state.video_track.peek().is_none());
        assert_eq!(state.current.peek().as_ref().unwrap().id, track.id);
        assert_eq!(services.core.load_playback().unwrap().position_seconds, 7.5);
        let bytes = mkv_fixture();
        std::fs::write(&track.path, &bytes).unwrap();
        dom.in_scope(ScopeId::ROOT, || state.retry_playback());
        assert!(state.playback_error.peek().is_none());
        tokio::time::timeout(Duration::from_secs(5), async {
            while *state.stream_loading.peek() {
                dom.render_immediate_to_vec();
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            state.playback_error.peek().is_none(),
            "{:?}",
            state.playback_error.peek()
        );
        assert_eq!(state.video_track.peek().as_ref().unwrap().id, track.id);
        assert!(state
            .video_url
            .peek()
            .as_ref()
            .unwrap()
            .starts_with("/preview/"));
        assert_eq!(*state.position.peek(), 7.5);
        assert_eq!(*state.duration.peek(), 12.0);
        assert!(*state.playing.peek());
        assert!(state.session_history.peek().is_empty());
        assert_eq!(services.core.recently_played(10).unwrap().len(), 1);
        assert_eq!(std::fs::read(&track.path).unwrap(), bytes);
        dom.in_scope(ScopeId::ROOT, || state.cancel_remote());
    }
}
