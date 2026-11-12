//! Favorites page and the favorite button. See `docs/systems/Favorites.md`.

use crate::{
    app::{EmptyState, TrackRow},
    icons::Icon,
    state::AppState,
};
use dioxus::prelude::*;
use yt_core::Track;

/// Heart toggle for one track. Never nest it inside another button.
#[component]
pub fn FavoriteButton(
    track: Track,
    #[props(default = 15)] size: u32,
    #[props(default = "")] class: &'static str,
) -> Element {
    let state = use_context::<AppState>();
    let favorite = state.is_favorite(&track.id);
    let label = if favorite {
        crate::favorites::REMOVE_LABEL
    } else {
        crate::favorites::ADD_LABEL
    };
    let modifier = if favorite { "favorite-button--on" } else { "" };
    rsx! {
        button {
            class: "favorite-button {modifier} {class}",
            r#type: "button",
            title: label,
            aria_label: "{label}: {track.title}",
            aria_pressed: "{favorite}",
            onclick: move |event| {
                event.stop_propagation();
                state.toggle_favorite(&track);
            },
            Icon {
                name: "heart",
                size,
                fill: if favorite { "currentColor" } else { "none" },
            }
        }
    }
}

#[component]
pub fn FavoritesPage() -> Element {
    let state = use_context::<AppState>();
    let loader = state.clone();
    let favorites = use_resource(move || {
        let core = loader.services.core.clone();
        loader.favorites_revision.read();
        async move {
            tokio::task::spawn_blocking(move || crate::favorites::load_favorite_tracks(&core))
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result)
        }
    });
    let loaded = favorites.read().clone();
    let tracks = match &loaded {
        Some(Ok(tracks)) => tracks.clone(),
        _ => Vec::new(),
    };
    let play_all = tracks.clone();
    let shuffled = tracks.clone();
    let play_state = state.clone();
    let shuffle_state = state.clone();
    let mut search_state = state.clone();
    rsx! {
        div { class: "page-shell",
            div { class: "favorites-hero",
                div { class: "favorites-hero__art", aria_hidden: "true",
                    Icon { name: "heart", size: 44, fill: "currentColor", stroke_width: 1.2 }
                }
                div { class: "favorites-hero__meta",
                    span { class: "favorites-hero__eyebrow", "Colección" }
                    h1 { class: "favorites-hero__title", "Favoritos" }
                    span { class: "favorites-hero__sub",
                        if tracks.len() == 1 { "1 canción" } else { "{tracks.len()} canciones" }
                    }
                    if !tracks.is_empty() {
                        div { class: "favorites-hero__actions",
                            button {
                                class: "btn-accent",
                                onclick: move |_| play_state.play_all(play_all.clone(), false),
                                Icon { name: "play", size: 13, fill: "currentColor" }
                                "Reproducir"
                            }
                            button {
                                class: "btn-secondary",
                                title: "Reproducir en orden aleatorio",
                                onclick: move |_| shuffle_state.play_all(shuffled.clone(), true),
                                Icon { name: "shuffle", size: 13 }
                            }
                        }
                    }
                }
            }
            match loaded {
                None => rsx! {
                    p { class: "rust-loading", role: "status", "Cargando favoritos…" }
                },
                Some(Err(error)) => rsx! {
                    EmptyState {
                        title: "No se pudieron cargar tus favoritos",
                        hint: error,
                        icon: "alert-circle",
                        compact: true,
                    }
                },
                Some(Ok(_)) if tracks.is_empty() => rsx! {
                    EmptyState {
                        title: "Aún no tienes favoritos",
                        hint: "Toca el corazón en cualquier canción, de tu biblioteca o de YouTube, para guardarla aquí.",
                        icon: "heart",
                        button {
                            class: "btn-accent",
                            onclick: move |_| search_state.page.set(crate::state::Page::Search),
                            Icon { name: "search", size: 14 }
                            "Buscar música"
                        }
                    }
                },
                Some(Ok(_)) => rsx! {
                    for (index, track) in tracks.into_iter().enumerate() {
                        TrackRow { key: "{track.id}", track, index }
                    }
                },
            }
        }
    }
}
