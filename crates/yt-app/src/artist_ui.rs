//! Artist page: the artist's banner, color and audience, their popular songs,
//! concerts, discography and biography from YouTube Music, and their music in
//! the library. See `docs/systems/Browse.md`.

use crate::{
    app::{artwork_src, TrackRow},
    artist_online::ArtistOnlineSections,
    cards::{CoverCard, DetailHero, Shelf},
    icons::Icon,
    navigation::{AlbumRef, ArtistRef, Detail},
    state::AppState,
    stations::{Station, StationKind},
};
use dioxus::prelude::*;
use yt_core::{artist_key, library_albums, Track};

/// Library songs listed on the page.
const LIBRARY_SONGS: usize = 10;
/// Songs that seed the artist's station.
const STATION_SEEDS: usize = 10;

/// Library tracks by `name`, newest first.
pub(crate) fn artist_tracks(library: &[Track], name: &str) -> Vec<Track> {
    let key = artist_key(name);
    library
        .iter()
        .filter(|track| !track.missing && artist_key(&track.artist) == key)
        .cloned()
        .collect()
}

/// A station seeded by the artist's songs.
pub(crate) fn artist_station(name: &str, seeds: Vec<Track>) -> Station {
    Station {
        key: format!("station:artist:{}", artist_key(name)),
        kind: StationKind::Artist,
        title: format!("Estación de {name}"),
        subtitle: format!("{name} y artistas parecidos"),
        seeds,
    }
}

/// «1 canción» / «N canciones» with a suffix.
fn count_label(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

#[component]
pub fn ArtistPage(artist: ArtistRef) -> Element {
    let state = use_context::<AppState>();
    let loader = state.clone();
    let online = use_resource(use_reactive((&artist,), move |(artist,)| {
        let state = loader.clone();
        async move { state.artist_profile(&artist).await }
    }));
    let (profile, failed) = match &*online.read() {
        Some(Ok(profile)) => (profile.clone(), false),
        Some(Err(_)) => (None, true),
        None => (None, false),
    };
    let loading = online.read().is_none();
    let name = profile
        .as_ref()
        .map_or(artist.name.clone(), |profile| profile.name.clone());
    let library = state.library.read().clone();
    let mut songs = artist_tracks(&library, &artist.name);
    if songs.is_empty() && name != artist.name {
        songs = artist_tracks(&library, &name);
    }
    let albums = library_albums(&songs);
    let channel = profile
        .as_ref()
        .map(|profile| profile.id.clone())
        .or_else(|| artist.channel_id.clone());
    let avatar = profile
        .as_ref()
        .map(|profile| profile.avatar.clone())
        .filter(|avatar| !avatar.is_empty())
        .or_else(|| (!artist.thumbnail.is_empty()).then(|| artist.thumbnail.clone()))
        .or_else(|| {
            songs
                .iter()
                .find(|track| !track.artwork_url.is_empty())
                .map(artwork_src)
        })
        .unwrap_or_default();
    let banner = profile
        .as_ref()
        .map(|profile| profile.banner.clone())
        .unwrap_or_default();
    let online_songs: Vec<Track> = profile
        .as_ref()
        .map(|profile| {
            profile
                .top_songs
                .iter()
                .map(|song| crate::remote::track_from_result(song.clone(), false))
                .collect()
        })
        .unwrap_or_default();
    let playable = if online_songs.is_empty() {
        songs.clone()
    } else {
        online_songs
    };
    let mut facts = Vec::new();
    if let Some(audience) = profile
        .as_ref()
        .map(|profile| profile.subscribers.clone())
        .filter(|text| !text.is_empty())
    {
        facts.push(audience);
    }
    if !songs.is_empty() {
        facts.push(count_label(
            songs.len(),
            "canción en tu biblioteca",
            "canciones en tu biblioteca",
        ));
    }
    if !albums.is_empty() {
        facts.push(count_label(
            albums.len(),
            "álbum en tu biblioteca",
            "álbumes en tu biblioteca",
        ));
    }
    let following = channel.as_deref().is_some_and(|id| state.is_following(id));
    let back = state.clone();
    let play = state.clone();
    let shuffle = state.clone();
    let station = state.clone();
    let follow = state.clone();
    let all = playable.clone();
    let shuffled = playable.clone();
    let seeds: Vec<Track> = songs
        .iter()
        .chain(playable.iter())
        .take(STATION_SEEDS)
        .cloned()
        .collect();
    let station_name = name.clone();
    let follow_name = name.clone();
    let follow_avatar = avatar.clone();
    let follow_channel = channel.clone();
    rsx! {
        div { class: "page-shell detail-page",
            DetailHero {
                title: name.clone(),
                eyebrow: "Artista",
                facts,
                artwork: avatar.clone(),
                banner,
                generated: Some(format!("artist:{}", artist_key(&name))),
                round: true,
                icon: "mic",
                onback: move |_| back.go_back(),
                if !playable.is_empty() {
                    button { class: "btn-accent", onclick: move |_| play.play_all(all.clone(), false),
                        Icon { name: "play", size: 13, fill: "currentColor" } "Reproducir"
                    }
                    button { class: "btn-secondary", title: "Aleatorio", aria_label: "Aleatorio", onclick: move |_| shuffle.play_all(shuffled.clone(), true),
                        Icon { name: "shuffle", size: 13 }
                    }
                }
                if !seeds.is_empty() {
                    button { class: "btn-secondary", onclick: move |_| station.play_station(artist_station(&station_name, seeds.clone())),
                        Icon { name: "radio", size: 13 } "Estación"
                    }
                }
                if let Some(id) = follow_channel {
                    button {
                        class: if following { "btn-secondary artist-follow artist-follow--on" } else { "btn-secondary artist-follow" },
                        onclick: move |_| follow.toggle_follow(&id, &follow_name, &follow_avatar),
                        Icon { name: if following { "user-check" } else { "user-plus" }, size: 13 }
                        if following { "Siguiendo" } else { "Seguir" }
                    }
                }
            }
            if let Some(profile) = profile {
                ArtistOnlineSections { profile }
            } else if loading {
                p { class: "rust-loading", role: "status", "Cargando el perfil de {name}…" }
            } else if failed {
                p { class: "rust-settings-note", "No se pudo cargar el perfil en línea; se muestra tu biblioteca." }
            }
            if !songs.is_empty() {
                h2 { class: "detail-section-title", "En tu biblioteca" }
                for (index, track) in songs.iter().take(LIBRARY_SONGS).cloned().enumerate() {
                    TrackRow { key: "{track.id}", track, index }
                }
            }
            if !albums.is_empty() {
                Shelf { title: "Álbumes en tu biblioteca",
                    for album in albums {
                        CoverCard {
                            key: "{album.title}",
                            title: album.title.clone(),
                            subtitle: album.artist.clone(),
                            artwork: album.artwork_track.as_ref().and_then(|id| songs.iter().find(|track| &track.id == id)).map(artwork_src).unwrap_or_default(),
                            icon: "album",
                            onopen: {
                                let state = state.clone();
                                let reference = AlbumRef {
                                    title: album.title.clone(),
                                    artist: album.artist.clone(),
                                    playlist_id: None,
                                    thumbnail: String::new(),
                                };
                                move |_| state.open_detail(Detail::Album(reference.clone()))
                            },
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_read_in_singular_and_plural() {
        assert_eq!(count_label(1, "álbum", "álbumes"), "1 álbum");
        assert_eq!(count_label(3, "álbum", "álbumes"), "3 álbumes");
    }
}
