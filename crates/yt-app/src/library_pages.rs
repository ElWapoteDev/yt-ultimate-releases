//! Library browse pages: Recientes, Artistas and Álbumes.
//! See `docs/systems/Browse.md`.

use crate::{
    app::{artwork_src, EmptyState},
    cards::{CoverCard, Shelf},
    navigation::{AlbumRef, Detail},
    state::AppState,
};
use dioxus::prelude::*;
use yt_core::{library_albums, library_artists, Track};

/// Items per Recientes shelf.
const RECENT_ITEMS: usize = 24;

fn artwork_of(id: Option<&String>, tracks: &[Track]) -> String {
    id.and_then(|id| tracks.iter().find(|track| &track.id == id))
        .map(artwork_src)
        .unwrap_or_default()
}

#[component]
pub fn RecentPage() -> Element {
    let state = use_context::<AppState>();
    let played = state
        .history
        .read()
        .iter()
        .take(RECENT_ITEMS)
        .cloned()
        .collect::<Vec<_>>();
    let added = state
        .library
        .read()
        .iter()
        .take(RECENT_ITEMS)
        .cloned()
        .collect::<Vec<_>>();
    rsx! {
        div { class: "page-shell",
            div { class: "page-header", h1 { class: "page-title", "Recientes" } }
            if played.is_empty() && added.is_empty() {
                EmptyState {
                    title: "Nada reciente todavía",
                    hint: "Lo que escuches y lo que añadas a tu biblioteca aparecerá aquí.",
                    icon: "clock",
                }
            }
            if !played.is_empty() {
                Shelf { title: "Escuchado recientemente",
                    for track in played {
                        TrackCard { key: "played-{track.id}", track }
                    }
                }
            }
            if !added.is_empty() {
                Shelf { title: "Añadido recientemente",
                    for track in added {
                        TrackCard { key: "added-{track.id}", track }
                    }
                }
            }
        }
    }
}

/// A song as a cover card: opens nothing, plays on click.
#[component]
pub fn TrackCard(track: Track) -> Element {
    let state = use_context::<AppState>();
    let active = state
        .current
        .read()
        .as_ref()
        .is_some_and(|current| current.id == track.id);
    let play = state.clone();
    let menu = state.clone();
    let played = track.clone();
    let menu_track = track.clone();
    rsx! {
        CoverCard {
            title: track.title.clone(),
            subtitle: track.artist.clone(),
            artwork: artwork_src(&track),
            active,
            onopen: move |_| {
                if active {
                    play.toggle_play();
                } else {
                    play.play(played.clone());
                }
            },
            oncontext: move |event| crate::context_menu::show(event, crate::context_menu::home_items(menu.clone(), menu_track.clone())),
        }
    }
}

#[component]
pub fn ArtistsPage() -> Element {
    let state = use_context::<AppState>();
    let tracks = state.library.read().clone();
    let artists = library_artists(&tracks);
    let followed = state.followed_artists.read().clone();
    rsx! {
        div { class: "page-shell",
            div { class: "toolbar",
                div { class: "toolbar__left",
                    h1 { class: "page-title", "Artistas" }
                    span { class: "page-count", "{artists.len() + followed.len()}" }
                }
            }
            if artists.is_empty() && followed.is_empty() {
                EmptyState {
                    title: "Aún no hay artistas",
                    hint: "Los artistas de tu biblioteca y los que sigas aparecerán aquí.",
                    icon: "mic",
                }
            }
            if !followed.is_empty() {
                Shelf { title: "Siguiendo",
                    for artist in followed {
                        CoverCard {
                            key: "{artist.id}",
                            title: artist.name.clone(),
                            subtitle: "Artista",
                            artwork: artist.thumbnail.clone(),
                            round: true,
                            icon: "mic",
                            onopen: {
                                let state = state.clone();
                                let artist = artist.clone();
                                move |_| state.open_artist(&artist.name, Some(artist.id.clone()), &artist.thumbnail)
                            },
                        }
                    }
                }
            }
            if !artists.is_empty() {
                h2 { class: "detail-section-title", "En tu biblioteca" }
                div { class: "browse-grid",
                    for artist in artists {
                        CoverCard {
                            key: "{artist.name}",
                            title: artist.name.clone(),
                            subtitle: if artist.track_ids.len() == 1 { "1 canción".to_owned() } else { format!("{} canciones", artist.track_ids.len()) },
                            artwork: artwork_of(artist.artwork_track.as_ref(), &tracks),
                            round: true,
                            icon: "mic",
                            onopen: {
                                let state = state.clone();
                                let name = artist.name.clone();
                                move |_| state.open_artist(&name, None, "")
                            },
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub fn AlbumsPage() -> Element {
    let state = use_context::<AppState>();
    let tracks = state.library.read().clone();
    let albums = library_albums(&tracks);
    rsx! {
        div { class: "page-shell",
            div { class: "toolbar",
                div { class: "toolbar__left",
                    h1 { class: "page-title", "Álbumes" }
                    span { class: "page-count", "{albums.len()}" }
                }
            }
            if albums.is_empty() {
                EmptyState {
                    title: "Aún no hay álbumes",
                    hint: "Las canciones con el álbum en sus etiquetas se agrupan aquí. Los álbumes de YouTube se abren desde la página de cada artista.",
                    icon: "album",
                }
            } else {
                div { class: "browse-grid",
                    for album in albums {
                        CoverCard {
                            key: "{album.artist}-{album.title}",
                            title: album.title.clone(),
                            subtitle: album.artist.clone(),
                            artwork: artwork_of(album.artwork_track.as_ref(), &tracks),
                            icon: "album",
                            onopen: {
                                let state = state.clone();
                                let reference = AlbumRef {
                                    title: album.title.clone(),
                                    artist: album.artist.clone(),
                                    playlist_id: None,
                                    thumbnail: artwork_of(album.artwork_track.as_ref(), &tracks),
                                };
                                move |_| state.open_detail(Detail::Album(reference.clone()))
                            },
                            onplay: {
                                let state = state.clone();
                                let ids = album.track_ids.clone();
                                let tracks = tracks.clone();
                                move |_| {
                                    let queue = ids.iter().filter_map(|id| tracks.iter().find(|track| &track.id == id).cloned()).collect();
                                    state.play_all(queue, false);
                                }
                            },
                        }
                    }
                }
            }
        }
    }
}
