//! Detail views opened above a page. See `docs/systems/Browse.md`.

use crate::navigation::Detail;
use dioxus::prelude::*;

#[component]
pub fn DetailView(view: Detail) -> Element {
    match view {
        Detail::Artist(artist) => rsx! { crate::artist_ui::ArtistPage { artist } },
        Detail::Album(album) => rsx! { crate::album_ui::AlbumPage { album } },
        Detail::Collection(collection) => rsx! { crate::home_ui::CollectionPage { collection } },
    }
}
