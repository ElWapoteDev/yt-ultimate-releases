//! Search result sections: the top result card and the online sections.
//! See `docs/systems/Search.md`.

use crate::{
    app::{artwork_src, Thumb},
    artist_online::{open_release, release_subtitle, ReleasesShelf},
    cards::{CoverCard, Shelf},
    favorites_ui::FavoriteButton,
    icons::Icon,
    search::TopResult,
    search_ui::SearchRow,
    state::{time_label, AppState},
};
use dioxus::prelude::*;
use yt_media::{AlbumSummary, ArtistSummary, CatalogSearch};

/// Compact view count: «1,2 M», «35 K»; empty for zero.
pub(crate) fn views_label(views: u64) -> String {
    const THOUSAND: u64 = 1_000;
    const MILLION: u64 = 1_000_000;
    const BILLION: u64 = 1_000_000_000;
    let scaled = |value: u64, unit: u64, suffix: &str| {
        format!("{:.1} {suffix}", value as f64 / unit as f64).replace('.', ",")
    };
    match views {
        0 => String::new(),
        value if value >= BILLION => scaled(value, BILLION, "mil M"),
        value if value >= MILLION => scaled(value, MILLION, "M"),
        value if value >= THOUSAND => format!("{} K", value / THOUSAND),
        value => value.to_string(),
    }
}

/// The best answer, large: artwork, kind, quick facts and actions.
#[component]
pub fn TopResultCard(top: TopResult) -> Element {
    let state = use_context::<AppState>();
    match &top {
        TopResult::LibraryArtist(name) => return rsx! { LibraryArtistCard { name: name.clone() } },
        TopResult::Artist(artist) => return rsx! { ArtistCard { artist: artist.clone() } },
        TopResult::Album(album) => return rsx! { AlbumCard { album: album.clone() } },
        _ => {}
    }
    let (track, kind, facts) = match &top {
        TopResult::Library(track) => (
            track.clone(),
            "Canción · En tu biblioteca",
            vec![if track.duration > 0.0 {
                time_label(track.duration)
            } else {
                String::new()
            }],
        ),
        TopResult::LibraryArtist(_) | TopResult::Artist(_) | TopResult::Album(_) => return rsx! {},
        TopResult::Song(song) => (
            crate::remote::track_from_result(song.clone(), false),
            "Canción · YouTube",
            vec![
                if song.duration_seconds > 0 {
                    time_label(song.duration_seconds as f64)
                } else {
                    String::new()
                },
                if song.view_count > 0 {
                    format!("{} reproducciones", views_label(song.view_count))
                } else {
                    String::new()
                },
            ],
        ),
    };
    let facts: Vec<String> = facts.into_iter().filter(|fact| !fact.is_empty()).collect();
    let active = state
        .current
        .read()
        .as_ref()
        .is_some_and(|current| current.id == track.id);
    let playing = active && *state.playing.read();
    let play = state.clone();
    let station = state.clone();
    let artist = state.clone();
    let chosen = top.clone();
    let seed = track.clone();
    let artist_name = track.artist.clone();
    rsx! {
        div { class: "search-top", style: "{crate::cards::cover_style(&track.id)}",
            div { class: "search-top__art",
                Thumb { src: artwork_src(&track), size: "square", icon_size: Some(40) }
            }
            span { class: "search-top__kind", "{kind}" }
            span { class: "search-top__title", title: "{track.title}", "{track.title}" }
            if !track.artist.is_empty() {
                button { class: "search-top__artist", onclick: move |_| artist.open_artist(&artist_name, None, ""), "{track.artist}" }
            }
            if !facts.is_empty() {
                div { class: "search-top__facts", for fact in facts { span { "{fact}" } } }
            }
            div { class: "search-top__actions",
                button {
                    class: "search-top__play",
                    title: if playing { "Pausar" } else { "Reproducir" },
                    onclick: move |_| match &chosen {
                        _ if active => play.toggle_play(),
                        TopResult::Library(track) => play.play(track.clone()),
                        TopResult::Song(song) => play.play_remote(song.clone(), false),
                        TopResult::LibraryArtist(_) | TopResult::Artist(_) | TopResult::Album(_) => {}
                    },
                    Icon { name: if playing { "pause" } else { "play" }, size: 18, fill: "currentColor", style: if playing { "" } else { "margin-left:2px" } }
                }
                button { class: "btn-secondary", title: "Música sin fin a partir de esta canción", onclick: move |_| station.start_song_station(&seed),
                    Icon { name: "radio", size: 13 } "Estación"
                }
                FavoriteButton { track: track.clone(), size: 18 }
            }
        }
    }
}

/// An artist of the library as the top result: their artwork, how many of
/// their songs the library holds, and the way to their page and station.
#[component]
fn LibraryArtistCard(name: String) -> Element {
    let state = use_context::<AppState>();
    let songs = crate::artist_ui::artist_tracks(&state.library.read(), &name);
    let artwork = songs
        .iter()
        .find(|track| !track.artwork_url.is_empty())
        .map(artwork_src)
        .unwrap_or_default();
    let count = match songs.len() {
        1 => "1 canción en tu biblioteca".to_owned(),
        count => format!("{count} canciones en tu biblioteca"),
    };
    let open = state.clone();
    let station = state.clone();
    let artist = name.clone();
    let seeds = songs.clone();
    rsx! {
        div { class: "search-top search-top--artist", style: "{crate::cards::cover_style(&name)}",
            button {
                class: "search-top__art search-top__art--round",
                aria_label: "Abrir {name}",
                onclick: move |_| open.open_artist(&artist, None, ""),
                Thumb { src: artwork, size: "square", icon_size: Some(40) }
            }
            span { class: "search-top__kind", "Artista · En tu biblioteca" }
            span { class: "search-top__title", title: "{name}", "{name}" }
            div { class: "search-top__facts", span { "{count}" } }
            div { class: "search-top__actions",
                button {
                    class: "search-top__play",
                    title: "Estación de {name}",
                    onclick: move |_| station.play_station(crate::artist_ui::artist_station(&name, seeds.clone())),
                    Icon { name: "radio", size: 18 }
                }
            }
        }
    }
}

/// An online artist as the top result: their photo, audience and the way
/// to their page.
#[component]
fn ArtistCard(artist: ArtistSummary) -> Element {
    let state = use_context::<AppState>();
    let open = state.clone();
    let chosen = artist.clone();
    let following = state.is_following(&artist.id);
    let follow = state.clone();
    let followed = artist.clone();
    rsx! {
        div { class: "search-top search-top--artist", style: "{crate::cards::cover_style(&artist.id)}",
            button {
                class: "search-top__art search-top__art--round",
                aria_label: "Abrir {artist.name}",
                onclick: move |_| open.open_artist(&chosen.name, Some(chosen.id.clone()), &chosen.thumbnail),
                Thumb { src: artist.thumbnail.clone(), size: "square", icon_size: Some(40) }
            }
            span { class: "search-top__kind", "Artista" }
            span { class: "search-top__title", title: "{artist.name}", "{artist.name}" }
            if !artist.subscribers.is_empty() {
                div { class: "search-top__facts", span { "{artist.subscribers}" } }
            }
            div { class: "search-top__actions",
                button {
                    class: if following { "btn-secondary" } else { "btn-accent" },
                    onclick: move |_| follow.toggle_follow(&followed.id, &followed.name, &followed.thumbnail),
                    Icon { name: if following { "user-check" } else { "user-plus" }, size: 13 }
                    if following { "Siguiendo" } else { "Seguir" }
                }
            }
        }
    }
}

/// An online album, single or EP as the top result.
#[component]
fn AlbumCard(album: AlbumSummary) -> Element {
    let state = use_context::<AppState>();
    let chosen = album.clone();
    rsx! {
        div { class: "search-top", style: "{crate::cards::cover_style(&album.id)}",
            button {
                class: "search-top__art search-top__art--button",
                aria_label: "Abrir {album.title}",
                onclick: move |_| open_release(&state, &chosen),
                Thumb { src: album.thumbnail.clone(), size: "square", icon_size: Some(40) }
            }
            span { class: "search-top__kind", "{release_subtitle(&album)}" }
            span { class: "search-top__title", title: "{album.title}", "{album.title}" }
            if !album.artist.is_empty() {
                span { class: "search-top__artist search-top__artist--static", "{album.artist}" }
            }
        }
    }
}

/// Every online section after the top row: artists, the remaining songs,
/// albums, playlists and videos. `onplaylist` opens a playlist by id.
#[component]
pub fn OnlineSections(found: CatalogSearch, onplaylist: EventHandler<String>) -> Element {
    let state = use_context::<AppState>();
    let rest: Vec<_> = found
        .songs
        .iter()
        .skip(crate::search::TOP_SONGS)
        .cloned()
        .collect();
    rsx! {
        if !found.artists.is_empty() {
            Shelf { title: "Artistas",
                for artist in found.artists.clone() {
                    CoverCard {
                        key: "{artist.id}",
                        title: artist.name.clone(),
                        subtitle: artist.subscribers.clone(),
                        artwork: artist.thumbnail.clone(),
                        generated: Some(format!("artist:{}", artist.id)),
                        icon: "mic",
                        round: true,
                        onopen: {
                            let state = state.clone();
                            let artist = artist.clone();
                            move |_| state.open_artist(&artist.name, Some(artist.id.clone()), &artist.thumbnail)
                        },
                    }
                }
            }
        }
        if !rest.is_empty() {
            h2 { class: "detail-section-title", "Más canciones" }
            for item in rest {
                SearchRow { key: "{item.id}", item }
            }
        }
        ReleasesShelf { title: "Álbumes", releases: found.albums.clone() }
        if !found.playlists.is_empty() {
            Shelf { title: "Playlists",
                for list in found.playlists.clone() {
                    CoverCard {
                        key: "{list.id}",
                        title: list.title.clone(),
                        subtitle: list.artist.clone(),
                        artwork: list.thumbnail.clone(),
                        generated: Some(format!("playlist:{}", list.id)),
                        icon: "playlist",
                        onopen: move |_| onplaylist.call(list.id.clone()),
                    }
                }
            }
        }
        if !found.videos.is_empty() {
            h2 { class: "detail-section-title", "Vídeos" }
            for item in found.videos.clone() {
                SearchRow { key: "video-{item.id}", item }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_counts_read_compactly_in_spanish() {
        assert_eq!(views_label(0), "");
        assert_eq!(views_label(950), "950");
        assert_eq!(views_label(35_400), "35 K");
        assert_eq!(views_label(1_250_000), "1,2 M");
        assert_eq!(views_label(2_100_000_000), "2,1 mil M");
    }
}
