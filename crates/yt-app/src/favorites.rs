//! Favorites controller: optimistic toggles for library and online media.
//! See `docs/systems/Favorites.md`.

use crate::{context_menu::Entry, state::AppState};
use dioxus::prelude::*;
use yt_core::{AppCore, CoreError, Track};

/// Action labels shared by the heart button and the menus.
pub(crate) const ADD_LABEL: &str = "Añadir a favoritos";
pub(crate) const REMOVE_LABEL: &str = "Quitar de favoritos";

/// Menu entry that toggles `track` as a favorite.
pub(crate) fn favorite_entry(state: &AppState, track: &Track) -> Entry {
    let favorite = state.favorites.peek().contains(&track.id);
    let state = state.clone();
    let track = track.clone();
    Entry::action(if favorite { REMOVE_LABEL } else { ADD_LABEL }, move || {
        state.toggle_favorite(&track)
    })
}

/// Every playable favorite, newest first. Library tracks whose file is
/// missing are left out because they cannot play. Runs off the UI thread.
pub(crate) fn load_favorite_tracks(core: &AppCore) -> Result<Vec<Track>, String> {
    let entries = core.favorites().map_err(|error| error.to_string())?;
    Ok(entries
        .iter()
        .map(|entry| {
            let mut track = crate::remote::track_from_item(&entry.item);
            track.favorite = true;
            track
        })
        .filter(|track| crate::remote::is_remote(track) || !track.missing)
        .collect())
}

impl AppState {
    /// Whether `id` is a favorite; subscribes the calling component.
    pub(crate) fn is_favorite(&self, id: &str) -> bool {
        self.favorites.read().contains(id)
    }

    /// Flip the favorite flag of `track`. The interface changes at once; a
    /// failed write puts it back and explains why.
    pub(crate) fn toggle_favorite(&self, track: &Track) {
        let favorite = !self.favorites.peek().contains(&track.id);
        self.show_favorite(&track.id, favorite);
        let result = if crate::remote::is_remote(track) {
            crate::remote::remote_media(track)
                .ok_or_else(|| CoreError::Invalid("La pista en línea no es válida".into()))
                .and_then(|media| self.services.core.set_remote_favorite(&media, favorite))
        } else {
            self.services.core.set_favorite(&track.id, favorite)
        };
        match result {
            Ok(()) => {
                let mut revision = self.favorites_revision;
                revision += 1;
            }
            Err(error) => {
                self.show_favorite(&track.id, !favorite);
                self.notify_error(format!("No se pudo actualizar tus favoritos: {error}"));
            }
        }
    }

    fn show_favorite(&self, id: &str, favorite: bool) {
        let mut favorites = self.favorites;
        if favorite {
            favorites.write().insert(id.to_owned());
        } else {
            favorites.write().remove(id);
        }
        let mut library = self.library;
        if let Some(track) = library.write().iter_mut().find(|track| track.id == id) {
            track.favorite = favorite;
        }
        let mut current = self.current;
        let mut playing = current.write();
        if let Some(track) = playing.as_mut().filter(|track| track.id == id) {
            track.favorite = favorite;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{remote_track, TestApp};

    #[tokio::test]
    async fn toggles_show_at_once_persist_and_roll_back_failed_writes() {
        let mut app = TestApp::new();
        let track = app.video_track("song");
        let core = app.services.core.clone();
        app.run(|state| {
            state.refresh();
            state.toggle_favorite(&track);
            assert!(state.favorites.peek().contains(&track.id));
            assert!(state
                .library
                .peek()
                .iter()
                .any(|item| item.id == track.id && item.favorite));
            assert!(core.favorite_ids().unwrap().contains(&track.id));
            state.toggle_favorite(&track);
            assert!(!state.favorites.peek().contains(&track.id));
            assert!(core.favorite_ids().unwrap().is_empty());
            let ghost = Track {
                id: "ghost".into(),
                ..track.clone()
            };
            state.toggle_favorite(&ghost);
            assert!(!state.favorites.peek().contains("ghost"));
            assert!(state.last_notice().unwrap().contains("favoritos"));
        });
    }

    #[tokio::test]
    async fn online_favorites_are_remembered_and_listed_with_library_ones() {
        let mut app = TestApp::new();
        let local = app.video_track("local");
        let online = remote_track("aqz-KE-bpKQ", "Online song");
        let core = app.services.core.clone();
        app.run(|state| {
            state.toggle_favorite(&local);
            state.toggle_favorite(&online);
            let favorite_entry_label = favorite_entry(state, &online).label;
            assert_eq!(favorite_entry_label, REMOVE_LABEL);
        });
        let tracks = load_favorite_tracks(&core).unwrap();
        assert_eq!(tracks.len(), 2);
        assert!(tracks
            .iter()
            .any(|track| track.id == online.id && crate::remote::is_remote(track)));
        assert!(tracks.iter().all(|track| track.favorite));
    }
}
