//! The online part of an artist page, from the cached YouTube Music profile:
//! popular songs, upcoming concerts, releases, videos, related artists and
//! the biography. See `docs/systems/Browse.md`.

use crate::{
    cards::{CoverCard, Shelf},
    concerts_ui::ConcertsSection,
    navigation::{AlbumRef, Detail},
    search_ui::SearchRow,
    state::AppState,
};
use dioxus::prelude::*;
use yt_media::{AlbumSummary, ArtistProfile, ReleaseKind};

/// Popular songs listed on the page.
const POPULAR_SONGS: usize = 10;
/// Characters of the biography shown before «Más».
const ABOUT_PREVIEW_CHARS: usize = 420;

/// Spanish name of a release kind.
pub(crate) fn release_label(kind: ReleaseKind) -> &'static str {
    match kind {
        ReleaseKind::Album => "Álbum",
        ReleaseKind::Single => "Sencillo",
        ReleaseKind::Ep => "EP",
        ReleaseKind::Unknown => "Lanzamiento",
    }
}

/// «Álbum · 2024».
pub(crate) fn release_subtitle(release: &AlbumSummary) -> String {
    match release.year {
        Some(year) => format!("{} · {year}", release_label(release.kind)),
        None => release_label(release.kind).to_owned(),
    }
}

/// Open `release` on its album page.
pub(crate) fn open_release(state: &AppState, release: &AlbumSummary) {
    state.open_detail(Detail::Album(AlbumRef {
        title: release.title.clone(),
        artist: release.artist.clone(),
        playlist_id: Some(release.id.clone()),
        thumbnail: release.thumbnail.clone(),
    }));
}

/// The first `limit` characters of `text` at a word boundary, and whether
/// anything was cut.
fn preview(text: &str, limit: usize) -> (String, bool) {
    if text.chars().count() <= limit {
        return (text.to_owned(), false);
    }
    let cut: String = text.chars().take(limit).collect();
    let end = cut.rfind(char::is_whitespace).unwrap_or(cut.len());
    (format!("{}…", cut[..end].trim_end()), true)
}

/// A shelf of releases that open their album page.
#[component]
pub fn ReleasesShelf(title: String, releases: Vec<AlbumSummary>) -> Element {
    let state = use_context::<AppState>();
    rsx! {
        if !releases.is_empty() {
            Shelf { title,
                for release in releases {
                    CoverCard {
                        key: "{release.id}",
                        title: release.title.clone(),
                        subtitle: release_subtitle(&release),
                        artwork: release.thumbnail.clone(),
                        generated: Some(format!("album:{}", release.id)),
                        icon: "album",
                        onopen: {
                            let state = state.clone();
                            let release = release.clone();
                            move |_| open_release(&state, &release)
                        },
                    }
                }
            }
        }
    }
}

#[component]
pub fn ArtistOnlineSections(profile: ArtistProfile) -> Element {
    let state = use_context::<AppState>();
    let mut expanded = use_signal(|| false);
    let (about, cut) = preview(&profile.description, ABOUT_PREVIEW_CHARS);
    let about = if *expanded.read() {
        profile.description.clone()
    } else {
        about
    };
    rsx! {
        if !profile.top_songs.is_empty() {
            h2 { class: "detail-section-title", "Populares" }
            div { class: "artist-popular",
                for item in profile.top_songs.iter().take(POPULAR_SONGS).cloned() {
                    SearchRow { key: "{item.id}", item }
                }
            }
        }
        ConcertsSection { artist: profile.name.clone(), events: profile.events.clone() }
        ReleasesShelf { title: "Álbumes", releases: profile.albums.clone() }
        ReleasesShelf { title: "Sencillos y EP", releases: profile.singles.clone() }
        if !profile.videos.is_empty() {
            Shelf { title: "Vídeos",
                for video in profile.videos.clone() {
                    CoverCard {
                        key: "{video.id}",
                        title: video.title.clone(),
                        subtitle: crate::search_results::views_label(video.view_count),
                        artwork: video.thumbnail.clone(),
                        icon: "video",
                        onopen: {
                            let state = state.clone();
                            let video = video.clone();
                            move |_| state.play_remote(video.clone(), true)
                        },
                    }
                }
            }
        }
        if !profile.related.is_empty() {
            Shelf { title: "Artistas similares",
                for artist in profile.related.clone() {
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
        if !about.trim().is_empty() {
            section { class: "artist-about",
                h2 { class: "detail-section-title", "Acerca de {profile.name}" }
                p { class: "detail-about", "{about}" }
                if cut {
                    button {
                        class: "section-action",
                        onclick: move |_| {
                            let open = !*expanded.peek();
                            expanded.set(open);
                        },
                        if *expanded.read() { "Menos" } else { "Más" }
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
    fn previews_cut_at_a_word_and_say_so() {
        assert_eq!(preview("Corta", 10), ("Corta".to_owned(), false));
        assert_eq!(
            preview("Una biografía muy larga", 15),
            ("Una biografía…".to_owned(), true)
        );
        assert_eq!(preview("Ñandúñandúñandú", 5), ("Ñandú…".to_owned(), true));
    }

    #[test]
    fn releases_read_as_their_kind_and_year() {
        let release = AlbumSummary {
            id: "OLAK5uy_abc".into(),
            title: "Génesis".into(),
            artist: "Peso Pluma".into(),
            year: Some(2023),
            kind: ReleaseKind::Album,
            thumbnail: String::new(),
            track_count: None,
        };
        assert_eq!(release_subtitle(&release), "Álbum · 2023");
        let single = AlbumSummary {
            year: None,
            kind: ReleaseKind::Single,
            ..release
        };
        assert_eq!(release_subtitle(&single), "Sencillo");
    }
}
