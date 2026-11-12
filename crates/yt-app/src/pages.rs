use dioxus::prelude::*;

#[component]
pub fn HomePage() -> Element {
    rsx! { crate::home_ui::HomePage {} }
}

#[component]
pub fn FavoritesPage() -> Element {
    rsx! { crate::favorites_ui::FavoritesPage {} }
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
