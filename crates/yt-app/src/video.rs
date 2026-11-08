//! Small DOM adapter for the system WebView media element. Playback state,
//! validation, persistence and queue decisions remain in Rust.
use crate::state::AppState;
use dioxus::prelude::*;
use serde_json::{json, Value};

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
            Err(error) if key(&state).as_ref() == Some(&media_key) => state.notify(error),
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
