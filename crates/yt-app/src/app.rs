use crate::{icons::Icon, pages::*, player::PlayerBar, state::*};
use dioxus::html::HasFileData;
use dioxus::prelude::*;
use std::rc::Rc;
use yt_core::Track;

#[component]
pub fn App() -> Element {
    let services = use_hook(|| Services::open().map(Rc::new));
    match services {
        Ok(services) => rsx! { Application { services } },
        Err(error) => rsx! {
            style { {include_str!("../assets/app.css")} }
            div { class: "page-shell", style: "background:#18181a;height:100vh",
                h1 { "No se pudo abrir YT Ultimate" }
                p { "{error}" }
                p { "Comprueba el acceso a la carpeta de la biblioteca y vuelve a abrir la aplicación." }
            }
        },
    }
}

#[derive(Props, Clone)]
struct ApplicationProps {
    services: Rc<Services>,
}
impl PartialEq for ApplicationProps {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.services, &other.services)
    }
}

#[allow(non_snake_case)]
fn Application(props: ApplicationProps) -> Element {
    let s = use_app_state(props.services);
    use_context_provider(|| s.clone());
    crate::native::use_native(s.clone());
    crate::local_media::use_local_media(s.services.core.clone());
    let startup = s.clone();
    use_hook(move || {
        startup.start_polling();
        let paths = std::env::args_os()
            .skip(1)
            .map(std::path::PathBuf::from)
            .filter(|p| p.exists())
            .collect::<Vec<_>>();
        if !paths.is_empty() {
            spawn(async move {
                startup.import_paths(paths).await;
            });
        }
    });
    let mut key_state = s.clone();
    let page = *s.page.read();
    let notice = s.notice.read().clone();
    let menu_track = s.menu_track.read().clone();
    let mut dismiss = s.clone();
    let drop_state = s.clone();
    rsx! {
        style { {include_str!("../assets/app.css")} }
        style { {include_str!("../assets/rust.css")} }
        div { class: "app-root", tabindex: "0",
            ondragover: |event| event.prevent_default(),
            ondrop: move |event| {
                event.prevent_default();
                let paths = event.files().iter().map(|file| file.path()).collect::<Vec<_>>();
                if !paths.is_empty() { let state = drop_state.clone(); spawn(async move { state.import_paths(paths).await; }); }
            },
            onkeydown: move |event| {
                let key = event.key();
                if key == Key::Escape { key_state.menu_track.set(None); key_state.queue_open.set(false); key_state.options_open.set(false); key_state.video_track.set(None); }
            },
            div { class: "app-body",
                Sidebar {}
                main { class: "main-wrap",
                    for current_page in [Page::Home, Page::Search, Page::Library, Page::Playlists, Page::Stems, Page::Downloads, Page::Settings] {
                        div { class: "page-host", hidden: page != current_page,
                            match current_page {
                                Page::Home => rsx! { HomePage {} }, Page::Search => rsx! { SearchPage {} },
                                Page::Library => rsx! { LibraryPage {} }, Page::Playlists => rsx! { PlaylistsPage {} },
                                Page::Downloads => rsx! { DownloadsPage {} }, Page::Settings => rsx! { SettingsPage {} },
                                Page::Stems => rsx! { StemsPage {} },
                            }
                        }
                    }
                    if *s.queue_open.read() { crate::player::QueuePanel {} }
                }
            }
            PlayerBar {}
            if s.video_track.read().is_some() { crate::player::VideoPanel {} }
            if let Some(track) = menu_track { TrackMenu { track } }
            if let Some(message) = notice {
                div { class: "rust-notice", role: "status", aria_live: "polite",
                    span { "{message}" }
                    button { class: "btn-icon-sm", aria_label: "Cerrar mensaje", onclick: move |_| dismiss.notice.set(None), Icon { name: "close" } }
                }
            }
        }
    }
}

#[component]
fn Sidebar() -> Element {
    let s = use_context::<AppState>();
    let active = *s.page.read();
    let advanced = s.settings.read().show_advanced_tools;
    rsx! {
        aside { class: "sidebar sidebar--opaque",
            div { class: "sidebar-drag" }
            nav { class: "sidebar-scroll", aria_label: "Navegación principal",
                NavButton { page: Page::Home, active }
                NavButton { page: Page::Search, active }
                div { class: "sidebar-section-label", "Biblioteca" }
                NavButton { page: Page::Library, active }
                NavButton { page: Page::Playlists, active }
                if advanced { NavButton { page: Page::Stems, active } }
                div { class: "sidebar-spacer" }
                NavButton { page: Page::Downloads, active }
                NavButton { page: Page::Settings, active }
            }
        }
    }
}

#[component]
fn NavButton(page: Page, active: Page) -> Element {
    let mut s = use_context::<AppState>();
    let badge = s
        .downloads
        .read()
        .iter()
        .filter(|d| matches!(d.status.as_str(), "Preparando" | "Descargando"))
        .count();
    rsx! {
        button { class: if page == active { "sidebar-item active" } else { "sidebar-item" }, aria_current: if page == active { "page" } else { "false" }, onclick: move |_| s.page.set(page),
            Icon { name: page.icon() }
            span { class: "sidebar-item__label", "{page.label()}" }
            if page == Page::Downloads && badge > 0 { span { class: "sidebar-badge", "{badge}" } }
        }
    }
}

#[component]
pub fn Thumb(
    #[props(default = String::new())] src: String,
    #[props(default = "sm")] size: &'static str,
) -> Element {
    let mut failed = use_signal(String::new);
    let failed_src = src.clone();
    let allowed = src.starts_with("https://")
        || src.starts_with("data:image/")
        || src.starts_with("/artwork/");
    rsx! { div { class: "thumb thumb--{size}",
        if allowed && *failed.read() != src {
            img { src, alt: "", loading: "lazy", onerror: move |_| failed.set(failed_src.clone()) }
        } else { div { class: "thumb-placeholder", Icon { name: "music", size: if size == "square" { 26 } else { 16 } } } }
    } }
}

pub fn artwork_src(track: &Track) -> String {
    if track.artwork_url.starts_with("https://") {
        track.artwork_url.clone()
    } else {
        format!("/artwork/{}", track.id)
    }
}

#[component]
pub fn EmptyState(
    title: String,
    hint: String,
    #[props(default = "music")] icon: &'static str,
    #[props(default)] children: Element,
) -> Element {
    rsx! { div { class: "empty-state", Icon { name: icon, size: 30 } span { class: "empty-state__title", "{title}" } span { class: "empty-state__hint", "{hint}" } {children} } }
}

#[component]
pub fn MediaCard(track: Track) -> Element {
    let mut s = use_context::<AppState>();
    let active = s.current.read().as_ref().is_some_and(|t| t.id == track.id);
    let t = track.clone();
    let menu = track.clone();
    let play_state = s.clone();
    rsx! { button { class: "media-card", title: "{track.title}",
        onclick: move |_| { if active { play_state.toggle_play(); } else { play_state.play(t.clone()); } },
        oncontextmenu: move |event| { event.prevent_default(); s.menu_track.set(Some(menu.clone())); },
        div { class: "card-cover", Thumb { src: crate::app::artwork_src(&track), size: "square" }
            span { class: "card-play", Icon { name: if active && *s.playing.read() { "pause" } else { "play" } } }
            if active && *s.playing.read() { span { class: "card-playing", Equalizer {} } }
        }
        div { class: if active { "media-card__title media-card__title--active" } else { "media-card__title" }, "{track.title}" }
        div { class: "media-card__sub", if track.artist.is_empty() { "{size_label(track.size_bytes)}" } else { "{track.artist}" } }
    } }
}

#[component]
pub fn Equalizer() -> Element {
    rsx! { span { class: "eq", aria_hidden: "true", span {} span {} span {} } }
}

#[component]
pub fn TrackRow(track: Track, #[props(default = 0)] index: usize) -> Element {
    let mut s = use_context::<AppState>();
    let active = s.current.read().as_ref().is_some_and(|t| t.id == track.id);
    let click = track.clone();
    let menu = track.clone();
    let more = track.clone();
    let row_state = s.clone();
    rsx! { div { class: if active { "row row--active" } else { "row" },
        oncontextmenu: move |event| { event.prevent_default(); s.menu_track.set(Some(menu.clone())); },
        button { class: "rust-track-main", onclick: move |_| { if active { row_state.toggle_play(); } else { row_state.play(click.clone()); } },
            span { class: "row__index", if active && *s.playing.read() { Equalizer {} } else { "{index + 1}" } }
            Thumb { src: crate::app::artwork_src(&track) }
            div { class: "row__meta", div { class: if active { "row__title row__title--active" } else { "row__title" }, "{track.title}" } div { class: "row__sub", "{track.artist}" } }
            span { class: "row__detail", "{time_label(track.duration)}" }
        }
        button { class: "btn-icon-sm", title: "Más opciones", aria_label: "Más opciones de {track.title}", onclick: move |_| s.menu_track.set(Some(more.clone())), Icon { name: "more" } }
    } }
}

#[component]
fn TrackMenu(track: Track) -> Element {
    let mut s = use_context::<AppState>();
    let mut close = s.clone();
    let play_state = s.clone();
    let play_track = track.clone();
    let mut queue_state = s.clone();
    let queue_track = track.clone();
    let favorite_state = s.clone();
    let favorite_track = track.clone();
    let remove_state = s.clone();
    let remove_track = track.clone();
    let playlists = s.playlists.read().clone();
    rsx! { div { class: "modal-backdrop", onclick: move |_| close.menu_track.set(None),
        div { class: "modal-panel rust-track-menu", role: "dialog", aria_label: "Opciones de canción", onclick: |e| e.stop_propagation(),
            div { class: "modal-header", div { class: "modal-title", "{track.title}" }
                button { class: "btn-icon-sm", title: "Cerrar", onclick: move |_| s.menu_track.set(None), Icon { name: "close" } }
            }
            button { class: "settings-row", onclick: move |_| { play_state.play(play_track.clone()); let mut menu = play_state.menu_track; menu.set(None); }, Icon { name: "play" } "Reproducir" }
            button { class: "settings-row", onclick: move |_| { queue_state.queue.write().insert(0, queue_track.clone()); queue_state.persist_playback(); queue_state.menu_track.set(None); }, Icon { name: "playlist" } "Reproducir a continuación" }
            button { class: "settings-row", onclick: move |_| {
                match favorite_state.services.core.set_favorite(&favorite_track.id, !favorite_track.favorite) { Ok(_) => { favorite_state.refresh(); let mut menu = favorite_state.menu_track; menu.set(None); }, Err(e) => favorite_state.notify(e.to_string()) }
            }, Icon { name: "heart" } if track.favorite { "Quitar de favoritos" } else { "Añadir a favoritos" } }
            if !playlists.is_empty() {
                div { class: "player-options__label", "Añadir a playlist" }
                for playlist in playlists {
                    PlaylistAction { playlist_id: playlist.id.clone(), name: playlist.name.clone(), track_id: track.id.clone() }
                }
            }
            div { class: "player-options__label", "Exportar audio" }
            for (label, format) in [("MP3", yt_audio::AudioFormat::Mp3(s.settings.read().audio_quality.parse().unwrap_or(320))), ("FLAC · 24 bits", yt_audio::AudioFormat::Flac), ("WAV · 24 bits", yt_audio::AudioFormat::Wav)] {
                button { class: "settings-row", disabled: *s.busy.read(), onclick: {
                    let mut state = s.clone(); let track = track.clone();
                    move |_| { state.menu_track.set(None); state.export_audio(track.clone(), format); }
                }, Icon { name: "download" } "{label}" }
            }
            if s.settings.read().show_advanced_tools {
                div { class: "player-options__label", "Procesar audio" }
                for (label, task) in [
                    ("Versión ardilla", crate::processing::ProcessingTask::Variant(yt_audio::VariantOptions::chipmunk(7.0, 1.0, 3.0), "Ardilla")),
                    ("Variación equilibrada", crate::processing::ProcessingTask::Variant(yt_audio::VariantOptions::variation(&track.id, false), "Variación")),
                ] {
                    button { class: "settings-row", onclick: { let mut state=s.clone(); let id=track.id.clone(); move |_| {state.menu_track.set(None); crate::processing::start_processing(state.clone(),id.clone(),task);} }, Icon { name:"scissors" } "{label}" }
                }
                if cfg!(feature="native-stems") {
                    for (label, task) in [("Separar vocales e instrumental",crate::processing::ProcessingTask::Stems),("CALIDAD",crate::processing::ProcessingTask::Quality)] {
                        button { class:"settings-row",onclick:{let mut state=s.clone();let id=track.id.clone();move |_|{state.menu_track.set(None);crate::processing::start_processing(state.clone(),id.clone(),task);}},Icon{name:"scissors"}"{label}" }
                    }
                }
            }
            div { class: "info-row", span { class: "info-row__label", "Duración" } span { class: "info-row__value", "{time_label(track.duration)}" } }
            div { class: "info-row", span { class: "info-row__label", "Archivo" } span { class: "info-row__value", "{track.filename}" } }
            div { class: "info-row", span { class: "info-row__label", "Tamaño" } span { class: "info-row__value", "{size_label(track.size_bytes)}" } }
            button { class: "settings-row rust-danger", onclick: move |_| {
                match remove_state.services.core.remove_track(&remove_track.id) { Ok(_) => { remove_state.refresh(); let mut menu = remove_state.menu_track; menu.set(None); remove_state.notify("Quitado de la biblioteca. El archivo original se conserva."); }, Err(e) => remove_state.notify(e.to_string()) }
            }, Icon { name: "trash" } "Quitar de la biblioteca" }
        }
    } }
}

#[component]
fn PlaylistAction(playlist_id: String, name: String, track_id: String) -> Element {
    let mut s = use_context::<AppState>();
    rsx! { button { class: "settings-row", onclick: move |_| {
        match s.services.core.add_to_playlist(&playlist_id, &track_id) { Ok(_) => { s.refresh(); s.menu_track.set(None); s.notify("Añadido a la playlist"); }, Err(e) => s.notify(e.to_string()) }
    }, Icon { name: "plus" } "{name}" } }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn harness() -> Element {
        let services = use_context::<Rc<Services>>();
        let state = use_app_state(services);
        use_context_provider(|| state);
        rsx! {
            Sidebar {}
            HomePage {}
            SearchPage {}
            LibraryPage {}
            PlaylistsPage {}
            DownloadsPage {}
            SettingsPage {}
            PlayerBar {}
        }
    }

    #[tokio::test]
    async fn all_rust_pages_render_from_an_empty_profile() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let mut dom = VirtualDom::new(harness);
        dom.insert_any_root_context(Box::new(services));
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        for label in [
            "Navegación principal",
            "Empieza tu biblioteca",
            "Buscar música",
            "Aún no hay música",
            "Sin playlists",
            "No hay descargas",
            "Ajustes",
            "Sin reproducción",
        ] {
            assert!(html.contains(label), "missing UI label: {label}");
        }
        assert!(html.contains("sidebar--opaque"));
        assert!(html.contains("player-bar"));
        assert!(!html.contains("http://127.0.0.1:8000"));
    }

    #[test]
    fn media_text_is_escaped_in_the_rust_renderer() {
        fn untrusted_text() -> Element {
            rsx! { EmptyState { title: "<script>alert(1)</script>", hint: "A & B" } }
        }
        let mut dom = VirtualDom::new(untrusted_text);
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert!(html.contains("&#60;script&#62;"), "rendered: {html}");
        assert!(!html.contains("<script>alert"));
    }
}
