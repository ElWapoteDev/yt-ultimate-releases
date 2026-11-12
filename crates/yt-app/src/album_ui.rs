//! Album page: cover and color, kind and year, the numbered track list, the
//! album's story and more from the artist, from the library and, for online
//! releases, from YouTube Music. See `docs/systems/Browse.md`.

use crate::{
    app::{artwork_src, EmptyState, TrackRow},
    artist_online::{release_label, ReleasesShelf},
    cards::DetailHero,
    icons::Icon,
    navigation::{AlbumRef, ArtistRef},
    search_ui::SearchRow,
    state::{time_label, AppState},
};
use dioxus::prelude::*;
use yt_core::{library_album, Track};
use yt_media::AlbumDetails;

/// Releases of the same artist listed under «Más de».
const MORE_RELEASES: usize = 12;

/// The library tracks of `album`, in album order.
pub(crate) fn album_tracks(library: &[Track], album: &AlbumRef) -> Vec<Track> {
    library_album(library, &album.title, &album.artist)
        .map(|found| {
            found
                .track_ids
                .iter()
                .filter_map(|id| library.iter().find(|track| &track.id == id).cloned())
                .collect()
        })
        .unwrap_or_default()
}

#[component]
pub fn AlbumPage(album: AlbumRef) -> Element {
    let state = use_context::<AppState>();
    let loader = state.clone();
    let online = use_resource(use_reactive((&album,), move |(album,)| {
        let state = loader.clone();
        async move {
            match album.playlist_id {
                Some(id) => state.album_details(&id).await.map(Some),
                None => Ok(None),
            }
        }
    }));
    let (details, failed) = match &*online.read() {
        Some(Ok(details)) => (details.clone(), false),
        Some(Err(_)) => (None, true),
        None => (None, false),
    };
    let loading = album.playlist_id.is_some() && online.read().is_none();
    let library = state.library.read().clone();
    let local = album_tracks(&library, &album);
    let online_tracks: Vec<Track> = details
        .as_ref()
        .map(|details| {
            details
                .tracks
                .iter()
                .map(|song| crate::remote::track_from_result(song.clone(), false))
                .collect()
        })
        .unwrap_or_default();
    let tracks = if online_tracks.is_empty() {
        local.clone()
    } else {
        online_tracks
    };
    let title = details
        .as_ref()
        .map_or(album.title.clone(), |details| details.title.clone());
    let artist_name = details
        .as_ref()
        .map_or(album.artist.clone(), |details| details.artist.clone());
    let artwork = details
        .as_ref()
        .map(|details| details.thumbnail.clone())
        .filter(|thumbnail| !thumbnail.is_empty())
        .or_else(|| (!album.thumbnail.is_empty()).then(|| album.thumbnail.clone()))
        .or_else(|| {
            local
                .iter()
                .find(|track| !track.artwork_url.is_empty())
                .map(artwork_src)
        })
        .unwrap_or_default();
    let eyebrow = details
        .as_ref()
        .map_or("Álbum", |details| release_label(details.kind));
    let duration: f64 = tracks.iter().map(|track| track.duration.max(0.0)).sum();
    let mut facts = Vec::new();
    if !artist_name.is_empty() {
        facts.push(artist_name.clone());
    }
    if let Some(year) = details.as_ref().and_then(|details| details.year) {
        facts.push(year.to_string());
    }
    if !tracks.is_empty() {
        facts.push(if tracks.len() == 1 {
            "1 canción".to_owned()
        } else {
            format!("{} canciones", tracks.len())
        });
    }
    if duration > 0.0 {
        facts.push(time_label(duration));
    }
    if !local.is_empty() && details.is_some() {
        facts.push("En tu biblioteca".to_owned());
    }
    let back = state.clone();
    let play = state.clone();
    let shuffle = state.clone();
    let station = state.clone();
    let open_artist = state.clone();
    let all = tracks.clone();
    let shuffled = tracks.clone();
    let seed = tracks.first().cloned();
    let artist_channel = details
        .as_ref()
        .and_then(|details| details.artist_id.clone());
    let artist_for_page = artist_name.clone();
    let channel_for_page = artist_channel.clone();
    rsx! {
        div { class: "page-shell detail-page",
            DetailHero {
                title: title.clone(),
                eyebrow,
                facts,
                artwork,
                generated: Some(format!("album:{}:{}", album.artist, album.title)),
                icon: "album",
                onback: move |_| back.go_back(),
                if !tracks.is_empty() {
                    button { class: "btn-accent", onclick: move |_| play.play_all(all.clone(), false),
                        Icon { name: "play", size: 13, fill: "currentColor" } "Reproducir"
                    }
                    button { class: "btn-secondary", title: "Aleatorio", aria_label: "Aleatorio", onclick: move |_| shuffle.play_all(shuffled.clone(), true),
                        Icon { name: "shuffle", size: 13 }
                    }
                }
                if let Some(seed) = seed {
                    button { class: "btn-secondary", onclick: move |_| station.start_song_station(&seed),
                        Icon { name: "radio", size: 13 } "Estación"
                    }
                }
                if !artist_name.is_empty() {
                    button {
                        class: "btn-secondary",
                        onclick: move |_| open_artist.open_artist(&artist_for_page, channel_for_page.clone(), ""),
                        Icon { name: "mic", size: 13 }
                        "Ver artista"
                    }
                }
            }
            if loading {
                p { class: "rust-loading", role: "status", "Cargando el álbum…" }
            } else if failed && local.is_empty() {
                p { class: "rust-settings-note", "No se pudo cargar el álbum en línea." }
            }
            if let Some(details) = details.as_ref() {
                div { class: "album-tracks",
                    for (index, item) in details.tracks.iter().cloned().enumerate() {
                        SearchRow { key: "{item.id}", item, index: Some(index) }
                    }
                }
            } else if !loading {
                if local.is_empty() {
                    EmptyState {
                        title: "No hay canciones de este álbum en tu biblioteca",
                        hint: "",
                        icon: "album",
                        compact: true,
                    }
                }
                for (index, track) in local.iter().cloned().enumerate() {
                    TrackRow { key: "{track.id}", track, index }
                }
            }
            if let Some(details) = details.as_ref().filter(|details| !details.description.trim().is_empty()) {
                section { class: "artist-about",
                    h2 { class: "detail-section-title", "Acerca de este álbum" }
                    p { class: "detail-about", "{details.description}" }
                }
            }
            if let (Some(details), Some(channel)) = (details.clone(), artist_channel) {
                MoreFromArtist { details, channel }
            }
        }
    }
}

/// Other releases of the album's artist, from their cached online page.
#[component]
fn MoreFromArtist(details: AlbumDetails, channel: String) -> Element {
    let state = use_context::<AppState>();
    let artist = ArtistRef {
        name: details.artist.clone(),
        channel_id: Some(channel),
        thumbnail: String::new(),
    };
    let profile = use_resource(use_reactive((&artist,), move |(artist,)| {
        let state = state.clone();
        async move { state.artist_profile(&artist).await.ok().flatten() }
    }));
    let releases: Vec<_> = profile
        .read()
        .as_ref()
        .and_then(Option::as_ref)
        .map(|profile| {
            profile
                .albums
                .iter()
                .chain(profile.singles.iter())
                .filter(|release| release.id != details.id)
                .take(MORE_RELEASES)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    rsx! {
        ReleasesShelf { title: format!("Más de {}", details.artist), releases }
    }
}
