//! Novedades: recent official uploads of the artists the user follows and
//! listens to most, and the artists they follow. See `docs/systems/Browse.md`.

use crate::{
    app::EmptyState,
    cards::CoverCard,
    discovery::{Shelf, ShelfKind},
    home_ui::SongShelf,
    state::AppState,
};
use dioxus::prelude::*;
use std::collections::HashSet;
use yt_core::{artist_key, FollowedArtist, Track};

/// Artists whose releases are looked up: followed ones first, then the most
/// listened.
const RELEASE_ARTISTS: usize = 8;
/// Songs per artist shelf.
const RELEASES_PER_ARTIST: usize = 12;
/// Songs in «Lo nuevo de tus artistas».
const LATEST_ITEMS: usize = 24;

/// Names to look up: followed artists, then the most listened, each once.
fn release_artists(followed: &[FollowedArtist], listened: &[String], limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    followed
        .iter()
        .map(|artist| artist.name.clone())
        .chain(listened.iter().cloned())
        .filter(|name| seen.insert(artist_key(name)))
        .take(limit)
        .collect()
}

/// The first song of every artist, then the second of every artist, and so
/// on, so the mix alternates artists. A collaboration listed by two artists
/// appears once.
fn alternate(lists: &[(String, Vec<Track>)], limit: usize) -> Vec<Track> {
    let longest = lists
        .iter()
        .map(|(_, tracks)| tracks.len())
        .max()
        .unwrap_or(0);
    let mut mix: Vec<Track> = (0..longest)
        .flat_map(|index| {
            lists
                .iter()
                .filter_map(move |(_, tracks)| tracks.get(index))
        })
        .cloned()
        .collect();
    crate::catalog::unique_ids(&mut mix, |track| &track.id);
    mix.truncate(limit);
    mix
}

#[component]
pub fn NewReleasesPage() -> Element {
    let state = use_context::<AppState>();
    let online = state.settings.read().discovery.online;
    let followed = state.followed_artists.read().clone();
    let loader = state.clone();
    let releases = use_resource(move || {
        let state = loader.clone();
        let followed = state.followed_artists.read().clone();
        let online = state.settings.read().discovery.online;
        async move {
            if !online {
                return Vec::new();
            }
            let taste = state.current_taste().await;
            let listened: Vec<String> = taste
                .profile
                .top_artists(RELEASE_ARTISTS)
                .iter()
                .map(|artist| artist.name.clone())
                .collect();
            let mut found = Vec::new();
            for name in release_artists(&followed, &listened, RELEASE_ARTISTS) {
                if let Ok(media) = state.artist_releases(&name).await {
                    let mut tracks: Vec<Track> =
                        media.iter().map(crate::remote::track_from_media).collect();
                    crate::catalog::unique_ids(&mut tracks, |track| &track.id);
                    tracks.truncate(RELEASES_PER_ARTIST);
                    if !tracks.is_empty() {
                        found.push((name, tracks));
                    }
                }
            }
            found
        }
    });
    let loading = online && releases.read().is_none();
    let lists = releases.read().clone().unwrap_or_default();
    let latest = alternate(&lists, LATEST_ITEMS);
    rsx! {
        div { class: "page-shell",
            div { class: "page-header", h1 { class: "page-title", "Novedades" } }
            if !online {
                EmptyState {
                    title: "Novedades necesita las recomendaciones en línea",
                    hint: "Actívalas en Ajustes → Inicio y recomendaciones para ver lanzamientos nuevos de tus artistas.",
                    icon: "sparkles",
                }
            }
            if loading {
                p { class: "rust-loading", role: "status", "Buscando lanzamientos de tus artistas…" }
            }
            if !latest.is_empty() {
                SongShelf {
                    shelf: Shelf {
                        kind: ShelfKind::MoreFromArtist,
                        key: "releases:latest".into(),
                        title: "Lo nuevo de tus artistas".into(),
                        subtitle: "Publicado por sus canales oficiales".into(),
                        tracks: latest,
                    },
                }
            }
            for (name, tracks) in lists {
                SongShelf {
                    key: "{name}",
                    shelf: Shelf {
                        kind: ShelfKind::MoreFromArtist,
                        key: format!("releases:{}", artist_key(&name)),
                        title: format!("Nuevo de {name}"),
                        subtitle: String::new(),
                        tracks,
                    },
                }
            }
            if !followed.is_empty() {
                h2 { class: "detail-section-title", "Artistas que sigues" }
                div { class: "browse-grid",
                    for artist in followed {
                        CoverCard {
                            key: "{artist.id}",
                            title: artist.name.clone(),
                            subtitle: "Artista",
                            artwork: artist.thumbnail.clone(),
                            generated: Some(format!("artist:{}", artist.id)),
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
            } else if online && !loading {
                p { class: "rust-settings-note", "Sigue a tus artistas desde su página para ver aquí primero sus lanzamientos." }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::remote_track;

    #[test]
    fn followed_artists_come_first_and_names_are_not_repeated() {
        let followed = vec![FollowedArtist {
            id: "UCabcdefghijklmnopqrstuv".into(),
            name: "Peso Pluma".into(),
            thumbnail: String::new(),
            followed_at: 0.0,
        }];
        let listened = vec![
            "peso pluma".to_owned(),
            "Morat".to_owned(),
            "Karol G".to_owned(),
        ];
        assert_eq!(
            release_artists(&followed, &listened, 2),
            ["Peso Pluma", "Morat"]
        );
    }

    #[test]
    fn the_latest_mix_alternates_artists() {
        let lists = vec![
            (
                "A".to_owned(),
                vec![
                    remote_track("aaaaaaaaaaa", "a1"),
                    remote_track("bbbbbbbbbbb", "a2"),
                ],
            ),
            ("B".to_owned(), vec![remote_track("ccccccccccc", "b1")]),
        ];
        let titles: Vec<String> = alternate(&lists, 10)
            .into_iter()
            .map(|track| track.title)
            .collect();
        assert_eq!(titles, ["a1", "b1", "a2"]);
    }

    #[test]
    fn a_collaboration_listed_by_two_artists_appears_once() {
        let together = remote_track("ddddddddddd", "Collab");
        let lists = vec![
            (
                "A".to_owned(),
                vec![together.clone(), remote_track("aaaaaaaaaaa", "a2")],
            ),
            (
                "B".to_owned(),
                vec![together, remote_track("ccccccccccc", "b2")],
            ),
        ];
        let titles: Vec<String> = alternate(&lists, 10)
            .into_iter()
            .map(|track| track.title)
            .collect();
        assert_eq!(titles, ["Collab", "a2", "b2"]);
    }
}
