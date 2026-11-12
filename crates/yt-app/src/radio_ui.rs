//! Radio page: stations made from the user's taste and stations by genre.
//! See `docs/systems/Browse.md`.

use crate::{
    cards::{CoverCard, GeneratedCover, Shelf},
    discovery::remote_tracks,
    icons::Icon,
    state::{AppState, Page},
    stations::{Station, StationKind},
};
use dioxus::prelude::*;

/// Seeds taken from a genre search.
const GENRE_SEEDS: usize = 6;

fn station_icon(kind: StationKind) -> &'static str {
    match kind {
        StationKind::Personal => "sparkles",
        StationKind::Artist => "mic",
        StationKind::Song => "music",
        StationKind::Mix => "disc",
        StationKind::Mood => "activity",
        StationKind::Genre => "radio",
    }
}

/// A station card with its generated, animated cover.
#[component]
pub fn StationCard(station: Station) -> Element {
    let state = use_context::<AppState>();
    let playing = state
        .autoplay
        .active_station
        .read()
        .as_ref()
        .is_some_and(|active| active.key == station.key);
    let start = station.clone();
    rsx! {
        CoverCard {
            title: station.title.clone(),
            subtitle: station.subtitle.clone(),
            generated: Some(station.key.clone()),
            icon: station_icon(station.kind),
            active: playing,
            onopen: move |_| state.play_station(start.clone()),
        }
    }
}

impl AppState {
    /// Start a genre station: its first search results become the seeds.
    pub(crate) fn play_genre_station(&self, label: &'static str, query: String) {
        let state = self.clone();
        dioxus::core::spawn_forever(async move {
            match state.search_media(&query).await {
                Ok(media) => {
                    let seeds = remote_tracks(&crate::discovery::candidates(&media), &media)
                        .into_iter()
                        .take(GENRE_SEEDS)
                        .collect();
                    state.play_station(Station {
                        key: format!("station:genre:{label}"),
                        kind: StationKind::Genre,
                        title: format!("Radio {label}"),
                        subtitle: "Estación por género".into(),
                        seeds,
                    });
                }
                Err(error) => state.notify_error(format!("No se pudo abrir la estación: {error}")),
            }
        });
    }
}

#[component]
pub fn RadioPage() -> Element {
    let state = use_context::<AppState>();
    let page = state.page;
    let refresher = state.clone();
    use_effect(move || {
        if *page.read() == Page::Radio {
            refresher.refresh_home(false);
        }
    });
    let stations = state.discovery.stations.read().clone();
    let online = state.settings.read().discovery.online;
    let personal = stations
        .iter()
        .find(|station| station.kind == StationKind::Personal)
        .cloned();
    let artists: Vec<Station> = stations
        .iter()
        .filter(|station| station.kind == StationKind::Artist)
        .cloned()
        .collect();
    let songs: Vec<Station> = stations
        .iter()
        .filter(|station| station.kind == StationKind::Song)
        .cloned()
        .collect();
    let mixes: Vec<Station> = stations
        .iter()
        .filter(|station| matches!(station.kind, StationKind::Mix | StationKind::Mood))
        .cloned()
        .collect();
    rsx! {
        div { class: "page-shell",
            style { {include_str!("radio_ui.css")} }
            div { class: "page-header", h1 { class: "page-title", "Radio" } }
            if let Some(station) = personal {
                RadioHero { station }
            }
            if !artists.is_empty() {
                Shelf { title: "Estaciones de tus artistas", subtitle: "Cada artista y lo que suena como él",
                    for station in artists { StationCard { key: "{station.key}", station } }
                }
            }
            if !mixes.is_empty() {
                Shelf { title: "Tus mezclas y estados de ánimo",
                    for station in mixes { StationCard { key: "{station.key}", station } }
                }
            }
            if !songs.is_empty() {
                Shelf { title: "A partir de tus canciones", subtitle: "Toca una para escuchar música parecida sin fin",
                    for station in songs { StationCard { key: "{station.key}", station } }
                }
            }
            if online {
                Shelf { title: "Explorar por género",
                    for (label, query) in crate::search_ui::genre_queries() {
                        CoverCard {
                            key: "{label}",
                            title: format!("Radio {label}"),
                            subtitle: "Estación",
                            generated: Some(format!("station:genre:{label}")),
                            icon: "radio",
                            onopen: {
                                let state = state.clone();
                                move |_| state.play_genre_station(label, query.clone())
                            },
                        }
                    }
                }
            }
            if stations.is_empty() && !online {
                crate::app::EmptyState {
                    title: "Escucha un poco para crear tus estaciones",
                    hint: "Las estaciones nacen de lo que escuchas. También puedes crear una desde cualquier canción con clic derecho → «Crear estación».",
                    icon: "radio",
                }
            }
        }
    }
}

#[component]
fn RadioHero(station: Station) -> Element {
    let state = use_context::<AppState>();
    let start = station.clone();
    rsx! {
        section { class: "radio-hero",
            div { class: "radio-hero__cover",
                GeneratedCover { cover: station.key.clone(), title: station.title.clone(), eyebrow: "Estación", icon: "sparkles" }
            }
            div { class: "radio-hero__meta",
                span { class: "radio-hero__eyebrow", "Hecha para ti" }
                h2 { class: "radio-hero__title", "{station.title}" }
                p { class: "radio-hero__sub", "{station.subtitle}" }
                button { class: "btn-accent", onclick: move |_| state.play_station(start.clone()),
                    Icon { name: "play", size: 13, fill: "currentColor" } "Escuchar"
                }
            }
        }
    }
}
