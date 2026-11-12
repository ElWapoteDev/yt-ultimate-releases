//! Stations: endless listening steered by seed songs. A station replaces
//! autoplay's seed while it plays and ends when the user starts something
//! else. See `docs/systems/Playback.md`.

use crate::{
    discovery::{candidates, library_candidates, library_tracks, remote_tracks, unix_now},
    state::AppState,
};
use dioxus::prelude::*;
use std::collections::HashSet;
use yt_core::Track;
use yt_discovery::{Candidate, RankContext};

/// Songs a station prepares per refill.
const STATION_BATCH: usize = 12;
/// Recent station songs never repeated within a refill.
const STATION_MEMORY: usize = 60;

/// What steers a station.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum StationKind {
    /// Built from everything the user likes.
    Personal,
    /// Started from one song.
    Song,
    /// An artist and artists like them.
    Artist,
    /// One of the user's taste clusters.
    Mix,
    /// A mood measured from audio.
    Mood,
    /// A genre or scene, seeded from a search.
    Genre,
}

/// A station ready to play.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Station {
    /// Stable identity; also the key of its generated cover.
    pub key: String,
    pub kind: StationKind,
    pub title: String,
    pub subtitle: String,
    /// Songs that start and steer the station, strongest first.
    pub seeds: Vec<Track>,
}

impl Station {
    /// A station from one song, as offered by «Crear estación».
    pub(crate) fn from_song(track: &Track) -> Self {
        Self {
            key: format!("station:song:{}", track.id),
            kind: StationKind::Song,
            title: format!("Estación de «{}»", track.title),
            subtitle: if track.artist.is_empty() {
                "Música parecida".into()
            } else {
                format!("{} y música parecida", track.artist)
            },
            seeds: vec![track.clone()],
        }
    }

    /// Main artist of the seeds, for continuity.
    fn seed_artist(&self) -> Option<&str> {
        self.seeds
            .first()
            .map(|track| track.artist.as_str())
            .filter(|artist| !artist.is_empty())
    }
}

impl AppState {
    /// Start `station` from its first seed and keep it going with picks
    /// steered by its seeds.
    pub(crate) fn play_station(&self, station: Station) {
        let Some(first) = station.seeds.first().cloned() else {
            self.notify_warning("Esta estación todavía no tiene canciones");
            return;
        };
        self.stop_station();
        let mut queue = self.queue;
        queue.set(Vec::new());
        self.clear_autoplay();
        *self.autoplay.station.borrow_mut() = Some(station.clone());
        let mut active = self.autoplay.active_station;
        active.set(Some(station));
        self.play_with(first, yt_media::Transition::Cut);
    }

    /// Start a station from `track` («Crear estación»).
    pub(crate) fn start_song_station(&self, track: &Track) {
        self.play_station(Station::from_song(track));
    }

    /// End the playing station; autoplay returns to its preference.
    pub(crate) fn stop_station(&self) {
        if self.autoplay.station.borrow_mut().take().is_some() {
            let mut active = self.autoplay.active_station;
            active.set(None);
            self.clear_autoplay();
        }
    }

    /// Picks for the active station after `current`: the provider's related
    /// music for the current song and for a seed, chosen by
    /// `yt_discovery::station_picks` for continuity with the seeds (artists
    /// and listening embeddings), a bounded share of exploration and
    /// diversity; the library when offline.
    pub(crate) async fn station_picks(&self, station: &Station, current: &Track) -> Vec<Track> {
        let taste = self.current_taste().await;
        let mut exclude: HashSet<String> = self
            .session_history
            .peek()
            .iter()
            .rev()
            .take(STATION_MEMORY)
            .map(|track| track.id.clone())
            .collect();
        exclude.insert(current.id.clone());
        exclude.extend(self.queue.peek().iter().map(|track| track.id.clone()));
        let context = RankContext {
            profile: &taste.profile,
            exclude: &exclude,
            seed_artist: station.seed_artist(),
            now: unix_now(),
            salt: rand::random(),
        };
        let seed_ids: Vec<String> = station.seeds.iter().map(|track| track.id.clone()).collect();
        let draws: u64 = rand::random();
        let pick = |pool: Vec<Candidate>| {
            yt_discovery::station_picks(
                pool,
                &taste.profile,
                &taste.beliefs,
                &taste.embeddings,
                &seed_ids,
                &context,
                draws,
                STATION_BATCH,
            )
        };
        if self.settings.peek().discovery.online {
            let mut pool = Vec::new();
            let seed = &station.seeds[rand::random_range(0..station.seeds.len())];
            for track in [current, seed] {
                if let Some(video_id) = self.seed_video_id(track).await {
                    if let Ok(media) = self.up_next_media(&video_id).await {
                        pool.extend(media);
                    }
                }
            }
            if !pool.is_empty() {
                let tracks = remote_tracks(&pick(candidates(&pool)), &pool);
                if !tracks.is_empty() {
                    return tracks;
                }
            }
        }
        let library = self.library.peek().clone();
        library_tracks(&pick(library_candidates(&library)), &library)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{remote_track, TestApp};

    #[test]
    fn song_stations_are_named_after_the_song_and_keyed_by_it() {
        let station = Station::from_song(&remote_track("aqz-KE-bpKQ", "Bailando"));
        assert_eq!(station.key, "station:song:youtube:aqz-KE-bpKQ");
        assert_eq!(station.title, "Estación de «Bailando»");
        assert_eq!(station.seed_artist(), Some("Channel"));
    }

    #[tokio::test]
    async fn a_station_takes_over_until_the_user_starts_something_else() {
        let mut app = TestApp::new();
        let first = app.video_track("first");
        let other = app.video_track("other");
        app.run(|state| {
            state.queue.set(vec![other.clone()]);
            state.start_song_station(&first);
            assert!(state.queue.peek().is_empty());
            assert!(state.autoplay.active_station.peek().is_some());
            assert!(state.autoplay_enabled());
            assert_eq!(state.current.peek().as_ref().unwrap().id, first.id);
            state.play(other.clone());
            assert!(state.autoplay.active_station.peek().is_none());
        });
    }
}
