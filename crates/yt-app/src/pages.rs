use crate::{
    app::{EmptyState, MediaCard},
    icons::Icon,
    state::*,
};
use chrono::Timelike;
use dioxus::prelude::*;

#[component]
pub fn HomePage() -> Element {
    let mut s = use_context::<AppState>();
    let hour = chrono::Local::now().hour();
    let greeting = match hour {
        6..=11 => "Buenos días",
        12..=19 => "Buenas tardes",
        _ => "Buenas noches",
    };
    let recent = s
        .library
        .read()
        .iter()
        .take(12)
        .cloned()
        .collect::<Vec<_>>();
    let history = s.history.read().iter().take(6).cloned().collect::<Vec<_>>();
    let mut library_nav = s.clone();
    rsx! { div { class: "page-shell",
        div { class: "home-hero", h1 { class: "home-hero__greeting", "{greeting}" } div { class: "home-hero__sub", "Tu música, sin distracciones." } }
        if recent.is_empty() {
            EmptyState { title: "Empieza tu biblioteca", hint: "Busca cualquier canción, artista o enlace de YouTube y descárgala para escucharla aquí, incluso sin conexión.",
                button { class: "btn-accent", onclick: move |_| s.page.set(Page::Search), Icon { name: "search", size: 14 } "Buscar música" }
            }
        }
        if !history.is_empty() {
            div { class: "section", div { class: "section-header", h2 { class: "section-title", "Escuchado recientemente" } }
                div { class: "media-grid", for track in history { MediaCard { key: "{track.id}", track,home:true } } }
            }
        }
        if !recent.is_empty() {
            div { class: "section", div { class: "section-header", h2 { class: "section-title", "Añadido recientemente" }
                button { class: "section-action", onclick: move |_| library_nav.page.set(Page::Library), "Ver todo" Icon { name: "arrow", size: 13 } }
            }
                div { class: "media-grid", for track in recent { MediaCard { key: "{track.id}", track,home:true } } }
            }
        }
    } }
}

#[component]
pub fn LibraryPage() -> Element {
    rsx! { crate::library_ui::LibraryPage {} }
}

#[component]
pub fn SearchPage() -> Element {
    rsx! {crate::search_ui::SearchPage{}}
}

#[component]
pub fn PlaylistsPage() -> Element {
    rsx! { crate::playlists_ui::PlaylistsPage {} }
}

#[component]
pub fn DownloadsPage() -> Element {
    rsx! { crate::downloads_ui::DownloadsPage {} }
}

#[component]
pub fn SettingsPage() -> Element {
    rsx! {crate::settings_ui::SettingsPage{}}
}

#[component]
pub fn StemsPage() -> Element {
    rsx! { crate::processing::ProcessingPage {} }
}
