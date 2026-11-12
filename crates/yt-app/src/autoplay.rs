//! Autoplay controller: keeps related music ready for when the queue runs
//! out. See `docs/systems/Playback.md`.

use crate::{
    discovery::{candidates, library_candidates, library_tracks, remote_tracks, unix_now},
    state::{use_app_signal, AppState},
};
use dioxus::prelude::*;
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
};
use yt_core::{ListenSource, Track};
use yt_discovery::{rank, RankContext, RankingConfig};

/// Suggestions prepared per refill.
const AUTOPLAY_BATCH: usize = 10;
/// A refill starts when fewer suggestions than this remain.
const AUTOPLAY_MIN_READY: usize = 2;
/// Recent session history excluded from suggestions.
const RECENT_EXCLUSION: usize = 50;

#[derive(Clone)]
pub(crate) struct Autoplay {
    /// Suggestions that play after the queue, in order.
    pub upcoming: Signal<Vec<Track>>,
    pub loading: Signal<bool>,
    /// The station that steers suggestions, for rendering.
    pub active_station: Signal<Option<crate::stations::Station>>,
    /// The same station for controllers, readable without subscribing.
    pub(crate) station: Rc<RefCell<Option<crate::stations::Station>>>,
    seed: Rc<RefCell<Option<String>>>,
    /// Id of the last suggestion that started; while it plays, the remaining
    /// suggestions stay instead of being rebuilt around it.
    last_taken: Rc<RefCell<Option<String>>>,
    generation: Rc<Cell<u64>>,
    start_when_ready: Rc<Cell<bool>>,
}

pub(crate) fn use_autoplay() -> Autoplay {
    Autoplay {
        upcoming: use_app_signal(Vec::new),
        loading: use_app_signal(|| false),
        active_station: use_app_signal(|| None),
        station: use_hook(|| Rc::new(RefCell::new(None))),
        seed: use_hook(|| Rc::new(RefCell::new(None))),
        last_taken: use_hook(|| Rc::new(RefCell::new(None))),
        generation: use_hook(|| Rc::new(Cell::new(0))),
        start_when_ready: use_hook(|| Rc::new(Cell::new(false))),
    }
}

impl AppState {
    /// Turn autoplay on or off and prepare or drop suggestions to match.
    pub(crate) fn set_autoplay(&self, enabled: bool) {
        self.edit_settings(|settings| settings.playback.autoplay = enabled);
        self.refresh_autoplay();
    }

    /// A playing station always continues; otherwise autoplay continues only
    /// after the queue, when the preference allows it and nothing repeats.
    pub(crate) fn autoplay_enabled(&self) -> bool {
        if *self.repeat.peek() != 0 {
            return false;
        }
        self.autoplay.station.borrow().is_some() || self.settings.peek().playback.autoplay
    }

    /// Make sure suggestions exist for the current track once the queue is
    /// empty. Cheap to call: it returns early when nothing needs to change.
    pub(crate) fn refresh_autoplay(&self) {
        let autoplay = self.autoplay.clone();
        if !self.autoplay_enabled() {
            self.clear_autoplay();
            return;
        }
        let Some(current) = self.current.peek().clone() else {
            return;
        };
        if !self.queue.peek().is_empty() {
            return;
        }
        let same_seed = autoplay.seed.borrow().as_deref() == Some(current.id.as_str());
        let continuing = autoplay.last_taken.borrow().as_deref() == Some(current.id.as_str());
        let enough = autoplay.upcoming.peek().len() >= AUTOPLAY_MIN_READY;
        if (same_seed || continuing) && (*autoplay.loading.peek() || enough) {
            return;
        }
        let generation = autoplay.generation.get().wrapping_add(1);
        autoplay.generation.set(generation);
        *autoplay.seed.borrow_mut() = Some(current.id.clone());
        let mut loading = autoplay.loading;
        loading.set(true);
        let state = self.clone();
        dioxus::core::spawn_forever(async move {
            let picks = state.autoplay_picks(&current).await;
            if state.autoplay.generation.get() != generation {
                return;
            }
            let mut loading = state.autoplay.loading;
            loading.set(false);
            let mut upcoming = state.autoplay.upcoming;
            upcoming.set(picks);
            let waiting = state.autoplay.start_when_ready.replace(false);
            let still_current = state
                .current
                .peek()
                .as_ref()
                .is_some_and(|track| track.id == current.id);
            if waiting && still_current && !*state.playing.peek() && state.queue.peek().is_empty() {
                if let Some(track) = state.take_autoplay() {
                    state.play_with(track, yt_media::Transition::Cut);
                }
            }
        });
    }

    /// Start the first suggestion as soon as one is ready.
    pub(crate) fn autoplay_when_ready(&self) {
        if !self.autoplay_enabled() {
            return;
        }
        self.autoplay.start_when_ready.set(true);
        self.refresh_autoplay();
    }

    /// Next suggestion without consuming it.
    pub(crate) fn peek_autoplay(&self) -> Option<Track> {
        if !self.autoplay_enabled() {
            return None;
        }
        self.autoplay.upcoming.peek().first().cloned()
    }

    /// Consume the next suggestion; its listen is attributed to autoplay.
    pub(crate) fn take_autoplay(&self) -> Option<Track> {
        if !self.autoplay_enabled() {
            return None;
        }
        let mut upcoming = self.autoplay.upcoming;
        let track = {
            let mut list = upcoming.write();
            (!list.is_empty()).then(|| list.remove(0))
        }?;
        self.listening
            .attribute_next(&track.id, ListenSource::Autoplay);
        *self.autoplay.last_taken.borrow_mut() = Some(track.id.clone());
        Some(track)
    }

    /// Drop suggestions and any pending refill.
    pub(crate) fn clear_autoplay(&self) {
        let autoplay = &self.autoplay;
        autoplay
            .generation
            .set(autoplay.generation.get().wrapping_add(1));
        autoplay.seed.borrow_mut().take();
        autoplay.last_taken.borrow_mut().take();
        autoplay.start_when_ready.set(false);
        let mut loading = autoplay.loading;
        loading.set(false);
        let mut upcoming = autoplay.upcoming;
        if !upcoming.peek().is_empty() {
            upcoming.set(Vec::new());
        }
    }

    async fn autoplay_picks(&self, seed: &Track) -> Vec<Track> {
        let station = self.autoplay.station.borrow().clone();
        if let Some(station) = station {
            return self.station_picks(&station, seed).await;
        }
        let taste = self.listening_taste().await;
        let mut exclude: HashSet<String> = self
            .session_history
            .peek()
            .iter()
            .rev()
            .take(RECENT_EXCLUSION)
            .map(|track| track.id.clone())
            .collect();
        exclude.insert(seed.id.clone());
        exclude.extend(self.queue.peek().iter().map(|track| track.id.clone()));
        let context = RankContext {
            profile: &taste.profile,
            exclude: &exclude,
            seed_artist: Some(&seed.artist),
            now: unix_now(),
            salt: rand::random(),
        };
        if self.settings.peek().discovery.online {
            if let Some(video_id) = self.seed_video_id(seed).await {
                if let Ok(media) = self.up_next_media(&video_id).await {
                    let mut exclude_seed = exclude.clone();
                    exclude_seed.insert(format!("{}{video_id}", yt_core::REMOTE_ID_PREFIX));
                    let online = RankContext {
                        exclude: &exclude_seed,
                        ..context
                    };
                    let ranked = rank(
                        candidates(&media),
                        &online,
                        &RankingConfig::online(),
                        AUTOPLAY_BATCH,
                    );
                    let tracks = remote_tracks(&ranked, &media);
                    if !tracks.is_empty() {
                        return tracks;
                    }
                }
            }
        }
        library_picks(&self.library.peek(), &context)
    }
}

/// Library suggestions for a seed when online discovery is off or failed.
fn library_picks(library: &[Track], context: &RankContext<'_>) -> Vec<Track> {
    let ranked = rank(
        library_candidates(library),
        context,
        &RankingConfig::library_autoplay(),
        AUTOPLAY_BATCH,
    );
    library_tracks(&ranked, library)
}

#[cfg(test)]
mod tests {
    use super::*;
    use yt_discovery::Profile;

    fn track(id: &str, artist: &str, media_type: &str) -> Track {
        Track {
            id: id.into(),
            title: format!("Song {id}"),
            artist: artist.into(),
            album: String::new(),
            filename: format!("{id}.m4a"),
            path: std::path::PathBuf::from(format!("/music/{id}.m4a")),
            media_type: media_type.into(),
            extension: "m4a".into(),
            duration: 200.0,
            size_bytes: 1,
            artwork_url: String::new(),
            favorite: false,
            missing: false,
            created_at: 0.0,
            source_url: String::new(),
        }
    }

    #[test]
    fn library_suggestions_follow_the_seed_artist_and_skip_excluded_and_videos() {
        let library = vec![
            track("seed", "Artist", "audio"),
            track("other", "Someone", "audio"),
            track("same", "Artist", "audio"),
            track("clip", "Artist", "video"),
        ];
        let profile = Profile::default();
        let exclude = HashSet::from(["seed".to_owned()]);
        let context = RankContext {
            profile: &profile,
            exclude: &exclude,
            seed_artist: Some("Artist"),
            now: 0.0,
            salt: 1,
        };
        let picks = library_picks(&library, &context);
        let ids: Vec<_> = picks.iter().map(|track| track.id.as_str()).collect();
        assert_eq!(ids, ["same", "other"]);
    }
}
