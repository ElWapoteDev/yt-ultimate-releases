use crate::state::AppState;
use dioxus::prelude::*;
use std::path::PathBuf;
use yt_core::Track;
use yt_media::{CancellationToken, SearchResult, StreamKind};

pub fn is_remote(track: &Track) -> bool {
    track.path.as_os_str().is_empty()
        && yt_media::canonical_video_url(&track.source_url)
            .ok()
            .is_some_and(|url| {
                url::Url::parse(&url)
                    .ok()
                    .and_then(|url| {
                        url.query_pairs()
                            .find(|(key, _)| key == "v")
                            .map(|(_, value)| value.into_owned())
                    })
                    .is_some_and(|id| track.id == format!("youtube:{id}"))
            })
}

pub(crate) fn track_from_result(item: SearchResult, video: bool) -> Track {
    Track {
        id: format!("youtube:{}", item.id),
        title: item.title,
        artist: item.channel,
        album: String::new(),
        filename: String::new(),
        path: PathBuf::new(),
        media_type: if video { "video" } else { "audio" }.into(),
        extension: if video { "mp4" } else { "m4a" }.into(),
        duration: item.duration_seconds as f64,
        size_bytes: 0,
        artwork_url: item.thumbnail,
        favorite: false,
        missing: false,
        created_at: chrono::Utc::now().timestamp() as f64,
        source_url: item.url,
    }
}

impl AppState {
    pub fn queue_remote(&self, item: SearchResult) {
        let mut queue = self.queue;
        queue.write().insert(0, track_from_result(item, false));
        self.persist_playback();
        self.notify("Se reproducirá a continuación");
    }
    pub fn play_remote(&self, item: SearchResult, video: bool) {
        if self.current.peek().as_ref().is_some_and(|track| {
            track.id == format!("youtube:{}", item.id) && (track.media_type == "video") == video
        }) {
            self.toggle_play();
            return;
        }
        let existing = self
            .library
            .peek()
            .iter()
            .find(|track| {
                !track.missing
                    && track.source_url == item.url
                    && (track.media_type == "video") == video
            })
            .cloned();
        if let Some(track) = existing {
            self.play(track);
            return;
        }
        self.start_remote(track_from_result(item, video), 0.0);
    }

    pub fn cancel_remote(&self) {
        let previous = self.stream_request.peek().clone();
        if let Some(previous) = previous {
            previous.cancel();
        }
        let session = self.stream_session.peek().clone();
        if let Some(session) = session {
            session.cancel();
        }
        let mut session_slot = self.stream_session;
        session_slot.set(None);
        crate::local_media::clear_prepared_video();
        let mut state = self.clone();
        state.stream_request.set(None);
        state.pending_seek.set(None);
        state.seek_busy.set(false);
        state.stream_loading.set(false);
        state.recovering.set(false);
        state.buffering.set(false);
        state.stream_generation += 1;
    }

    pub fn start_remote(&self, track: Track, position: f64) {
        self.start_remote_attempt(track, position, 0);
    }

    fn start_remote_attempt(&self, track: Track, position: f64, attempt: u8) {
        if !is_remote(&track) {
            self.notify("El vídeo guardado no tiene una dirección válida de YouTube");
            return;
        }
        self.cancel_remote();
        let mut state = self.clone();
        let cancel = CancellationToken::new();
        state.stream_request.set(Some(cancel.clone()));
        state.stream_loading.set(true);
        state.buffering.set(true);
        state.recovering.set(attempt > 0);
        state.recovery_attempts.set(attempt);
        state.playback_error.set(None);
        state.video_track.set(None);
        state.video_url.set(None);
        state.current.set(Some(track.clone()));
        state.duration.set(track.duration);
        state.position.set(position.max(0.0));
        state.playing.set(false);
        if let Ok(audio) = &state.services.audio {
            let _ = audio.stop();
        }
        let generation = *state.stream_generation.peek();
        dioxus::core::spawn_forever(async move {
            let result = async {
                let youtube = state.services.youtube_client().map_err(|error| error.to_string())?;
                let video = track.media_type == "video";
                let resolved = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err("Reproducción cancelada".to_owned()),
                    result = youtube.resolve(&track.source_url, if video { StreamKind::Video } else { StreamKind::Audio }) => result,
                };
                if cancel.is_cancelled() || *state.stream_generation.peek() != generation {
                    return Err("Reproducción cancelada".into());
                }
                if video {
                    let (media, source) = match resolved {
                        Ok(resolved) => (resolved.media, resolved.stream_url),
                        Err(yt_media::MediaError::FormatUnavailable) => {
                            state.notify("Preparando vídeo…");
                            let quality = state.settings.peek().video_quality.parse::<u32>().map(yt_media::VideoQuality::MaxHeight).unwrap_or(yt_media::VideoQuality::MaxHeight(1080));
                            let prepared = youtube.prepare_video(&track.source_url, quality, cancel.clone(), |_| {}).await.map_err(|error|error.to_string())?;
                            if cancel.is_cancelled() || *state.stream_generation.peek() != generation { return Err("Reproducción cancelada".into()); }
                            let media=prepared.media.clone();
                            let source=crate::local_media::register_prepared_video(std::sync::Arc::new(prepared));
                            (media,source)
                        }
                        Err(error) => return Err(error.to_string()),
                    };
                    let remote = track_from_result(media, true);
                    state.video_url.set(Some(source));
                    state.video_track.set(Some(remote.clone()));
                    state.video_generation += 1;
                    state.video_open.set(true);
                    Ok::<Track,String>(remote)
                } else {
                    let resolved=resolved.map_err(|error|error.to_string())?;
                    let remote=track_from_result(resolved.media.clone(),false);
                    let audio = state.services.audio.as_ref().map_err(Clone::clone)?;
                    audio.set_volume(*state.volume.peek()).map_err(|error|error.to_string())?;
                    audio.set_speed(*state.rate.peek()).map_err(|error|error.to_string())?;
                    let session = tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return Err("Reproducción cancelada".to_owned()),
                        result = audio.play_stream(resolved, cancel.clone()) => result.map_err(|error|error.to_string())?,
                    };
                    if cancel.is_cancelled() || *state.stream_generation.peek()!=generation { session.cancel(); return Err("Reproducción cancelada".into()); }
                    state.stream_session.set(Some(session.clone()));
                    let position = *state.position.peek();
                    if position > 0.0 {
                        let seek_session=session.clone();
                        let result=tokio::task::spawn_blocking(move || seek_session.seek(position)).await;
                        if let Err(error)=result.map_err(|error|error.to_string()).and_then(|value|value.map_err(|error|error.to_string())) { session.cancel(); return Err(error); }
                    }
                    Ok::<Track,String>(remote)
                }
            }.await;
            if *state.stream_generation.peek() != generation {
                return;
            }
            state.stream_loading.set(false);
            state.buffering.set(false);
            state.recovering.set(false);
            match result {
                Ok(track) => {
                    state.duration.set(track.duration);
                    state.current.set(Some(track.clone()));
                    state.playing.set(true);
                    state.history.write().retain(|entry| entry.id != track.id);
                    state.history.write().insert(0, track);
                    state.persist_playback();
                }
                Err(error) => {
                    state.playing.set(false);
                    if !cancel.is_cancelled() {
                        state.recover_remote(error);
                    }
                }
            }
        });
    }
    pub fn recover_remote(&self, error: String) {
        if *self.stream_loading.peek() || *self.recovering.peek() {
            return;
        }
        let current = self.current.peek().clone();
        let mut state = self.clone();
        state.playing.set(false);
        let Some(track) = current.filter(is_remote) else {
            if state.playback_error.peek().as_ref() != Some(&error) {
                state.playback_error.set(Some(error));
            }
            return;
        };
        let attempt = *state.recovery_attempts.peek();
        if attempt >= 2 {
            if state.playback_error.peek().as_ref() != Some(&error) {
                state.playback_error.set(Some(error.clone()));
                state.notify(format!("No se pudo reproducir: {error}"));
            }
            return;
        }
        let position = *state.position.peek();
        state.cancel_remote();
        state.recovering.set(true);
        state.stream_loading.set(true);
        state.buffering.set(true);
        state.playing.set(false);
        let token = CancellationToken::new();
        state.stream_request.set(Some(token.clone()));
        let generation = *state.stream_generation.peek();
        dioxus::core::spawn_forever(async move {
            tokio::select! {biased;_=token.cancelled()=>return, _=tokio::time::sleep(std::time::Duration::from_millis(500*(attempt as u64+1)))=>{}}
            if *state.stream_generation.peek() == generation && !token.is_cancelled() {
                state.start_remote_attempt(track, position, attempt + 1);
            }
        });
    }

    pub fn seek_remote(&self, seconds: f64) {
        let mut state = self.clone();
        state.pending_seek.set(Some(seconds));
        state.position.set(seconds);
        state.persist_playback();
        if *state.seek_busy.peek() {
            return;
        }
        state.seek_busy.set(true);
        let generation = *state.stream_generation.peek();
        dioxus::core::spawn_forever(async move {
            loop {
                if *state.stream_generation.peek() != generation {
                    break;
                }
                let target = state.pending_seek.peek().to_owned();
                state.pending_seek.set(None);
                let Some(target) = target else {
                    break;
                };
                let Some(session) = state.stream_session.peek().clone() else {
                    break;
                };
                let result = tokio::task::spawn_blocking(move || session.seek(target)).await;
                if *state.stream_generation.peek() != generation {
                    break;
                }
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        state.notify(format!("No se pudo cambiar la posición: {error}"))
                    }
                    Err(error) => state.notify(format!("No se pudo cambiar la posición: {error}")),
                }
            }
            if *state.stream_generation.peek() != generation {
                return;
            }
            state.seek_busy.set(false);
            let queued = state.pending_seek.peek().to_owned();
            if let Some(target) = queued {
                state.seek_remote(target);
            }
        });
    }
}
