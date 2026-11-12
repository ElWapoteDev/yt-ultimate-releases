//! Listening controller: opens, measures, and closes one listening event for
//! the media that is actually playing. See `docs/systems/Discovery.md`.

use crate::state::AppState;
use dioxus::prelude::*;
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};
use yt_core::{
    AudioFeatures, ListenEnd, ListenId, ListenProgress, ListenSource, ListenTarget, Track,
};
use yt_discovery::ListenMeter;

/// How often an open event's progress is written, so a crash or forced quit
/// loses at most this much listening.
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(15);

struct Session {
    id: ListenId,
    media_id: String,
    duration: f64,
    meter: ListenMeter,
    last_sample: Instant,
    last_checkpoint: Instant,
    /// Latest sound measurement the playback engine published for it.
    sound: Option<AudioFeatures>,
}

impl Session {
    fn progress(&self) -> ListenProgress {
        ListenProgress {
            listened_seconds: self.meter.heard_seconds(),
            duration: self.duration,
        }
    }
}

/// Shared listening state; cloned with `AppState`.
#[derive(Clone, Default)]
pub(crate) struct Listening {
    session: Rc<RefCell<Option<Session>>>,
    /// Who chose the next playback, for that exact media id.
    next_source: Rc<RefCell<Option<(String, ListenSource)>>>,
}

impl Listening {
    /// Attribute the next listening event of `media_id` to `source`
    /// (autoplay marks its picks this way right before starting them).
    pub(crate) fn attribute_next(&self, media_id: &str, source: ListenSource) {
        *self.next_source.borrow_mut() = Some((media_id.to_owned(), source));
    }
}

impl AppState {
    /// Start measuring `track`; any open event ends as interrupted first.
    pub(crate) fn begin_listening(&self, track: &Track) {
        self.end_listening(false);
        let source = self
            .listening
            .next_source
            .borrow_mut()
            .take()
            .filter(|(media_id, _)| *media_id == track.id)
            .map_or(ListenSource::User, |(_, source)| source);
        let remote = crate::remote::remote_media(track);
        let target = match &remote {
            Some(media) => ListenTarget::Remote(media),
            None if !track.path.as_os_str().is_empty() => ListenTarget::Local(&track.id),
            None => return,
        };
        match self.services.core.begin_listen(target, source) {
            Ok(id) => {
                let now = Instant::now();
                *self.listening.session.borrow_mut() = Some(Session {
                    id,
                    media_id: track.id.clone(),
                    duration: track.duration,
                    meter: ListenMeter::default(),
                    last_sample: now,
                    last_checkpoint: now,
                    sound: None,
                });
            }
            Err(error) => self.log_listening_error(&error),
        }
    }

    /// Sample playback once; called from the shared playback poll.
    pub(crate) fn listening_tick(&self) {
        let current = self.current.peek().as_ref().map(|track| track.id.clone());
        let position = *self.position.peek();
        let playing = *self.playing.peek();
        let rate = f64::from(*self.rate.peek());
        let duration = *self.duration.peek();
        let checkpoint = {
            let mut slot = self.listening.session.borrow_mut();
            let Some(session) = slot.as_mut() else {
                return;
            };
            if current.as_deref() != Some(session.media_id.as_str()) {
                return;
            }
            let now = Instant::now();
            let elapsed = now.duration_since(session.last_sample).as_secs_f64();
            session.meter.observe(position, playing, elapsed, rate);
            session.last_sample = now;
            if duration > 0.0 {
                session.duration = duration;
            }
            (now.duration_since(session.last_checkpoint) >= CHECKPOINT_INTERVAL).then(|| {
                session.last_checkpoint = now;
                (session.id, session.progress())
            })
        };
        if let Some((id, progress)) = checkpoint {
            if let Err(error) = self.services.core.record_listen_progress(id, progress) {
                self.log_listening_error(&error);
            }
        }
    }

    /// Remember the engine's latest sound measurement of the playing media;
    /// called from the shared playback poll for audio the engine plays.
    pub(crate) fn observe_sound(&self, sound: Option<AudioFeatures>) {
        let Some(sound) = sound else {
            return;
        };
        let current = self.current.peek().as_ref().map(|track| track.id.clone());
        if let Some(session) = self.listening.session.borrow_mut().as_mut() {
            if current.as_deref() == Some(session.media_id.as_str()) {
                session.sound = Some(sound);
            }
        }
    }

    /// Close the open event, if any. `finished` means the media reached its
    /// natural end (or an automatic transition began at its end).
    pub(crate) fn end_listening(&self, finished: bool) {
        let Some(session) = self.listening.session.borrow_mut().take() else {
            return;
        };
        if let Some(sound) = &session.sound {
            self.keep_sound(&session.media_id, sound);
        }
        let end = if finished {
            ListenEnd::Finished
        } else {
            ListenEnd::Interrupted
        };
        if let Err(error) = self
            .services
            .core
            .end_listen(session.id, session.progress(), end)
        {
            self.log_listening_error(&error);
        }
    }

    /// Erase the listening history and everything derived from it (Settings →
    /// Privacidad). Favorites stay.
    pub(crate) fn erase_listening_history(&self) {
        self.listening.session.borrow_mut().take();
        match self.services.core.clear_listening_history() {
            Ok(erased) => {
                let mut history = self.history;
                history.set(Vec::new());
                let mut session_history = self.session_history;
                session_history.set(Vec::new());
                self.album_art.forget();
                self.forget_dj_analyses();
                self.reload_search_history();
                self.persist_playback();
                self.clear_autoplay();
                self.reset_home();
                self.reset_lyrics();
                self.refresh_home(true);
                self.notify_success(match erased.events {
                    1 => "Historial borrado: 1 reproducción".to_owned(),
                    count => format!("Historial borrado: {count} reproducciones"),
                });
            }
            Err(error) => self.notify_error(format!("No se pudo borrar el historial: {error}")),
        }
    }

    /// History is bookkeeping: a failed write is logged, never shown as an
    /// interruption of what the user is doing.
    fn log_listening_error(&self, error: &yt_core::CoreError) {
        crate::native::record_log(
            &self.services.profile,
            &format!("Listening history was not updated: {error}"),
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::TestApp;
    use dioxus::prelude::*;
    use yt_core::{ListenEnd, ListenSource};

    #[tokio::test]
    async fn one_event_per_playback_closed_as_finished_or_interrupted() {
        let mut app = TestApp::new();
        let first = app.video_track("first");
        let second = app.video_track("second");
        let core = app.services.core.clone();
        app.run(|state| {
            state.queue.set(vec![second.clone()]);
            state.play(first.clone());
            state.next(true);
            state
                .listening
                .attribute_next(&first.id, ListenSource::Autoplay);
            state.play(first.clone());
            state.next(false);
        });
        let signals = core.listening_signals(10).unwrap();
        assert_eq!(signals.len(), 3);
        assert_eq!(signals[2].media_id, first.id);
        assert_eq!(signals[2].end, ListenEnd::Finished);
        assert_eq!(signals[1].media_id, second.id);
        assert_eq!(signals[1].end, ListenEnd::Interrupted);
        assert_eq!(signals[0].source, ListenSource::Autoplay);
        assert_eq!(signals[0].end, ListenEnd::Interrupted);
    }

    #[tokio::test]
    async fn erasing_history_clears_every_derived_list_but_keeps_favorites() {
        let mut app = TestApp::new();
        let track = app.video_track("kept");
        let core = app.services.core.clone();
        app.run(|state| {
            state.play(track.clone());
            state.toggle_favorite(&track);
            state.erase_listening_history();
            assert!(state.history.peek().is_empty());
            assert!(state.session_history.peek().is_empty());
            assert!(state.last_notice().unwrap().contains("Historial borrado"));
        });
        assert!(core.listening_signals(10).unwrap().is_empty());
        assert_eq!(core.favorite_ids().unwrap().len(), 1);
    }
}
