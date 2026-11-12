//! Following artists: optimistic toggles like favorites.
//! See `docs/systems/Browse.md`.

use crate::state::AppState;
use dioxus::prelude::*;
use yt_core::FollowedArtist;

impl AppState {
    /// Whether the channel is followed; subscribes the calling component.
    pub(crate) fn is_following(&self, channel_id: &str) -> bool {
        self.followed_artists
            .read()
            .iter()
            .any(|artist| artist.id == channel_id)
    }

    /// Follow or unfollow an artist. The button changes at once; a failed
    /// write puts it back and explains why.
    pub(crate) fn toggle_follow(&self, id: &str, name: &str, thumbnail: &str) {
        let following = self
            .followed_artists
            .peek()
            .iter()
            .any(|artist| artist.id == id);
        let artist = FollowedArtist {
            id: id.to_owned(),
            name: name.trim().to_owned(),
            thumbnail: thumbnail.to_owned(),
            followed_at: crate::discovery::unix_now(),
        };
        let mut list = self.followed_artists;
        let previous = list.peek().clone();
        if following {
            list.write().retain(|known| known.id != id);
        } else {
            list.write().insert(0, artist.clone());
        }
        let result = if following {
            self.services.core.unfollow_artist(id)
        } else {
            self.services.core.follow_artist(&artist)
        };
        if let Err(error) = result {
            list.set(previous);
            self.notify_error(format!("No se pudo actualizar a quién sigues: {error}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::TestApp;
    use dioxus::prelude::*;

    #[tokio::test]
    async fn follows_toggle_at_once_and_invalid_artists_roll_back() {
        let mut app = TestApp::new();
        let core = app.services.core.clone();
        app.run(|state| {
            let id = "UCabcdefghijklmnopqrstuv";
            state.toggle_follow(id, "Artist", "");
            assert!(state
                .followed_artists
                .peek()
                .iter()
                .any(|artist| artist.id == id));
            assert_eq!(core.followed_artists().unwrap().len(), 1);
            state.toggle_follow(id, "Artist", "");
            assert!(state.followed_artists.peek().is_empty());
            state.toggle_follow("not-a-channel", "Artist", "");
            assert!(state.followed_artists.peek().is_empty());
            assert!(state.last_notice().unwrap().contains("sigues"));
        });
    }
}
