//! OS media callbacks are marshalled onto the Dioxus UI task; they never access signals from a native thread.
use super::*;
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};
use std::{cell::RefCell, rc::Rc};

pub(super) fn use_media_controls(state: AppState) {
    let window = use_window();
    let (controls, events) = use_hook({
        let state = state.clone();
        let window = window.clone();
        move || {
            #[cfg(target_os = "windows")]
            let hwnd = {
                use dioxus::desktop::tao::platform::windows::WindowExtWindows;
                Some(window.window.hwnd() as *mut std::ffi::c_void)
            };
            #[cfg(not(target_os = "windows"))]
            let hwnd = {
                let _ = window;
                None
            };
            let (sender, receiver) = tokio::sync::mpsc::channel(128);
            let result = MediaControls::new(PlatformConfig {
                dbus_name: "com.ytultimate.desktop",
                display_name: "YT Ultimate",
                hwnd,
            })
            .and_then(|mut controls| {
                controls.attach(move |event| {
                    let _ = sender.try_send(event);
                })?;
                Ok(controls)
            });
            let controls = match result {
                Ok(controls) => Some(controls),
                Err(error) => {
                    state.notify(format!(
                        "No se pudieron activar los controles multimedia del sistema: {error}"
                    ));
                    None
                }
            };
            (
                Rc::new(RefCell::new(controls)),
                Rc::new(RefCell::new(Some(receiver))),
            )
        }
    });
    use_hook({
        let state = state.clone();
        move || {
            let mut receiver = events
                .borrow_mut()
                .take()
                .expect("one native media receiver");
            spawn(async move {
                while let Some(event) = receiver.recv().await {
                    handle(&state, event);
                }
            });
        }
    });
    let metadata_controls = controls.clone();
    let metadata_state = state.clone();
    use_effect(move || {
        let track = metadata_state.current.read().clone();
        let duration = seconds(*metadata_state.duration.read());
        let Some(controls) = &mut *metadata_controls.borrow_mut() else {
            return;
        };
        let cover = track
            .as_ref()
            .and_then(|track| cover_url(&metadata_state, track));
        let result = controls.set_metadata(MediaMetadata {
            title: Some(track.as_ref().map_or("", |track| track.title.as_str())),
            artist: Some(track.as_ref().map_or("", |track| track.artist.as_str())),
            album: Some(track.as_ref().map_or("", |track| track.album.as_str())),
            cover_url: cover.as_deref(),
            duration: Some(duration),
        });
        if let Err(error) = result {
            record_log(
                &metadata_state.services.profile,
                &format!("OS media metadata: {error}"),
            );
        }
    });
    let playback_controls = controls.clone();
    use_effect(move || {
        let present = state.current.read().is_some();
        let playing = *state.playing.read();
        let position = seconds(*state.position.read());
        let shuffle = *state.shuffle.read();
        let Some(controls) = &mut *playback_controls.borrow_mut() else {
            return;
        };
        let playback = if !present {
            MediaPlayback::Stopped
        } else if playing {
            MediaPlayback::Playing {
                progress: Some(MediaPosition(position)),
            }
        } else {
            MediaPlayback::Paused {
                progress: Some(MediaPosition(position)),
            }
        };
        if let Err(error) = controls.set_playback(playback) {
            record_log(
                &state.services.profile,
                &format!("OS media playback: {error}"),
            );
        }
        #[cfg(target_os = "macos")]
        if let Err(error) = controls.set_shuffle(Some(shuffle)) {
            record_log(
                &state.services.profile,
                &format!("OS media shuffle: {error}"),
            );
        }
        #[cfg(not(target_os = "macos"))]
        let _ = shuffle;
    });
    use_drop(move || {
        if let Some(mut controls) = controls.borrow_mut().take() {
            let _ = controls.set_playback(MediaPlayback::Stopped);
            let _ = controls.detach();
        }
    });
}

fn seconds(value: f64) -> Duration {
    Duration::from_secs_f64(if value.is_finite() {
        value.clamp(0.0, 31_536_000.0)
    } else {
        0.0
    })
}
fn cover_url(state: &AppState, track: &yt_core::Track) -> Option<String> {
    if let Ok(Some(path)) = state.services.core.artwork_path(&track.id) {
        #[cfg(target_os = "windows")]
        {
            // Souvlaki passes the portion after file:// directly to StorageFile;
            // it does not URL-decode local Windows paths with spaces.
            let path = path.to_string_lossy();
            let path = if let Some(unc) = path.strip_prefix("\\\\?\\UNC\\") {
                format!("\\\\{unc}")
            } else {
                path.strip_prefix("\\\\?\\").unwrap_or(&path).to_owned()
            };
            return Some(format!("file://{path}"));
        }
        #[cfg(not(target_os = "windows"))]
        return url::Url::from_file_path(path).ok().map(String::from);
    }
    let url = url::Url::parse(&track.artwork_url).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.into())
}
fn handle(state: &AppState, event: MediaControlEvent) {
    let mut state = state.clone();
    match event {
        MediaControlEvent::Play => {
            let playing = *state.playing.peek();
            let loading = *state.stream_loading.peek();
            if !playing && !loading {
                state.toggle_play();
            }
        }
        MediaControlEvent::Pause => {
            let playing = *state.playing.peek();
            let loading = *state.stream_loading.peek();
            if playing || loading {
                state.toggle_play();
            }
        }
        MediaControlEvent::Toggle => state.toggle_play(),
        MediaControlEvent::Next => state.next(false),
        MediaControlEvent::Previous => state.previous(),
        MediaControlEvent::Seek(direction) => seek_by(&state, direction, Duration::from_secs(10)),
        MediaControlEvent::SeekBy(direction, duration) => seek_by(&state, direction, duration),
        MediaControlEvent::SetPosition(position) => state.seek(position.0.as_secs_f64()),
        MediaControlEvent::SetVolume(volume) if volume.is_finite() => {
            state.set_volume(volume.clamp(0.0, 1.0) as f32)
        }
        MediaControlEvent::SetShuffle(shuffle) => {
            let current = *state.shuffle.peek();
            if current != shuffle {
                state.toggle_shuffle();
            }
        }
        MediaControlEvent::Stop => {
            state.cancel_remote();
            if state.video_track.peek().is_some() {
                crate::video::command(state.clone(), serde_json::json!({"op":"pause"}));
            }
            if let Ok(audio) = &state.services.audio {
                let _ = audio.stop();
            }
            state.video_track.set(None);
            state.video_url.set(None);
            state.video_open.set(false);
            state.playing.set(false);
            state.position.set(0.0);
            state.persist_playback();
        }
        MediaControlEvent::Raise => {
            let window = dioxus::desktop::window();
            window.window.set_minimized(false);
            window.window.set_visible(true);
            window.window.set_focus();
        }
        MediaControlEvent::Quit => request_close(&state),
        MediaControlEvent::OpenUri(uri) => {
            if let Ok(path) = url::Url::parse(&uri).and_then(|url| {
                url.to_file_path()
                    .map_err(|_| url::ParseError::RelativeUrlWithoutBase)
            }) {
                if let Ok(path) = openable_file(&path) {
                    dioxus::core::spawn_forever(async move {
                        state.import_paths(vec![path]).await;
                    });
                }
            }
        }
        _ => {}
    }
}
fn seek_by(state: &AppState, direction: SeekDirection, duration: Duration) {
    let position = *state.position.peek();
    let delta = duration.as_secs_f64();
    state.seek(match direction {
        SeekDirection::Forward => position + delta,
        SeekDirection::Backward => (position - delta).max(0.0),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_position_sanitizes_unavailable_or_invalid_values() {
        assert_eq!(seconds(f64::NAN), Duration::ZERO);
        assert_eq!(seconds(-1.0), Duration::ZERO);
        assert_eq!(seconds(1.25), Duration::from_millis(1250));
    }
}
