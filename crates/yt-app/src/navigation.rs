//! Navigation between pages and detail views (artist, album, collection),
//! with a back stack like a music app's back button.

use crate::{
    context_menu::Entry,
    state::{AppState, Page},
};
use dioxus::prelude::*;
use yt_core::Track;

/// Menu labels shared by every track menu that offers these actions.
pub(crate) const STATION_LABEL: &str = "Crear estación";
pub(crate) const ARTIST_LABEL: &str = "Ir al artista";
pub(crate) const ALBUM_LABEL: &str = "Ir al álbum";

/// «Crear estación», «Ir al artista», «Ir al álbum» and «Ver créditos» for
/// `track`. The artist and album entries appear when the track names them or
/// has a YouTube identity through which YouTube Music can name them.
pub(crate) fn browse_entries(state: &AppState, track: &Track) -> Vec<Entry> {
    let station = state.clone();
    let seed = track.clone();
    let mut entries = vec![Entry::action(STATION_LABEL, move || {
        station.start_song_station(&seed)
    })];
    let online = crate::remote::video_id_of(track).is_some();
    if online || !track.artist.trim().is_empty() {
        let artist = state.clone();
        let song = track.clone();
        entries.push(Entry::action(ARTIST_LABEL, move || {
            artist.open_track_artist(&song)
        }));
    }
    if online || !track.album.trim().is_empty() {
        let album = state.clone();
        let song = track.clone();
        entries.push(Entry::action(ALBUM_LABEL, move || {
            album.open_track_album(&song)
        }));
    }
    let credits = state.clone();
    let song = track.clone();
    entries.push(Entry::action(crate::credits_ui::CREDITS_LABEL, move || {
        credits.show_credits(&song)
    }));
    entries
}

/// Deepest back stack kept; older entries fall off.
const MAX_DETAIL_DEPTH: usize = 24;

/// An artist to show: a library name, an online channel, or both.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ArtistRef {
    pub name: String,
    /// YouTube channel id when known (validated by `yt_core::is_channel_id`).
    pub channel_id: Option<String>,
    pub thumbnail: String,
}

/// An album to show: a library album, an online release, or both.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AlbumRef {
    pub title: String,
    pub artist: String,
    /// YouTube playlist id of the release when known.
    pub playlist_id: Option<String>,
    pub thumbnail: String,
}

/// A view shown above the current page.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Detail {
    Artist(ArtistRef),
    Album(AlbumRef),
    /// A made-for-you collection from Home, by its key.
    Collection(String),
}

impl AppState {
    /// Show `page` and close every detail view.
    pub(crate) fn navigate(&self, page: Page) {
        let mut detail = self.detail;
        if !detail.peek().is_empty() {
            detail.set(Vec::new());
        }
        let mut current = self.page;
        current.set(page);
    }

    /// Open a detail view above the current page; opening the view already
    /// on top does nothing.
    pub(crate) fn open_detail(&self, view: Detail) {
        let mut detail = self.detail;
        if detail.peek().last() == Some(&view) {
            return;
        }
        let mut stack = detail.write();
        stack.push(view);
        if stack.len() > MAX_DETAIL_DEPTH {
            stack.remove(0);
        }
    }

    /// Close the top detail view.
    pub(crate) fn go_back(&self) {
        let mut detail = self.detail;
        detail.write().pop();
    }

    /// Show one playlist on the Playlists page.
    pub(crate) fn open_playlist(&self, id: &str) {
        let mut selected = self.selected_playlist;
        selected.set(Some(id.to_owned()));
        self.navigate(Page::Playlists);
    }

    pub(crate) fn open_artist(&self, name: &str, channel_id: Option<String>, thumbnail: &str) {
        self.open_detail(Detail::Artist(ArtistRef {
            name: name.trim().to_owned(),
            channel_id: channel_id.filter(|id| yt_core::is_channel_id(id)),
            thumbnail: thumbnail.to_owned(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{remote_track, TestApp};

    #[tokio::test]
    async fn online_songs_can_open_their_artist_album_and_credits() {
        let mut app = TestApp::new();
        let untagged = app.video_track("untagged");
        app.run(|state| {
            let labels = |track: &Track| {
                browse_entries(state, track)
                    .iter()
                    .map(|entry| entry.label.clone())
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                labels(&remote_track("abcdefghijk", "Bailando")),
                [
                    STATION_LABEL,
                    ARTIST_LABEL,
                    ALBUM_LABEL,
                    crate::credits_ui::CREDITS_LABEL
                ]
            );
            assert_eq!(
                labels(&untagged),
                [STATION_LABEL, crate::credits_ui::CREDITS_LABEL]
            );
        });
    }

    #[tokio::test]
    async fn details_stack_above_pages_and_navigation_clears_them() {
        let mut app = TestApp::new();
        app.run(|state| {
            let artist = Detail::Artist(ArtistRef {
                name: "Artist".into(),
                channel_id: None,
                thumbnail: String::new(),
            });
            state.open_detail(artist.clone());
            state.open_detail(artist.clone());
            assert_eq!(state.detail.peek().len(), 1);
            state.open_detail(Detail::Collection("weekly".into()));
            state.go_back();
            assert_eq!(state.detail.peek().last(), Some(&artist));
            state.navigate(Page::Library);
            assert!(state.detail.peek().is_empty());
            assert_eq!(*state.page.peek(), Page::Library);
            for index in 0..(MAX_DETAIL_DEPTH + 5) {
                state.open_detail(Detail::Collection(format!("c{index}")));
            }
            assert_eq!(state.detail.peek().len(), MAX_DETAIL_DEPTH);
            state.open_artist("Name", Some("not-a-channel".into()), "");
            assert!(matches!(
                state.detail.peek().last(),
                Some(Detail::Artist(ArtistRef {
                    channel_id: None,
                    ..
                }))
            ));
        });
    }
}
