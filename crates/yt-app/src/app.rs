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
    crate::updates::use_updates(s.clone());
    let menus = crate::context_menu::use_menus();
    let importing = crate::context_menu::use_document_controls(s.clone(), menus.clone());
    let edit_state = s.clone();
    let edit_menus = menus.clone();
    let window = try_consume_context::<dioxus::desktop::DesktopContext>();
    use_effect(|| {
        let script = if cfg!(target_os = "windows") {
            "document.documentElement.dataset.platform='win32';"
        } else {
            "document.documentElement.dataset.platform='darwin';"
        };
        document::eval(script);
    });
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
    let mut dismiss = s.clone();
    let drop_state = s.clone();
    rsx! {
        style { {include_str!("../assets/app.css")} }
        style { {include_str!("../assets/rust.css")} }
        div { class: "app-root", tabindex: "0",
            oncontextmenu:move|event|crate::context_menu::show_edit(event,edit_state.clone(),edit_menus.clone()),
            ondragover: |event| event.prevent_default(),
            ondrop: move |event| {
                event.prevent_default();
                if crate::native::outgoing_media_drag_active() { return; }
                let paths = event.files().iter().map(|file| file.path()).collect::<Vec<_>>();
                if !paths.is_empty() { let state = drop_state.clone(); spawn(async move { state.import_paths(paths).await; }); }
            },
            onkeydown: move |event| {
                let key = event.key();
                if key == Key::Escape { menus.dismiss(); key_state.menu_track.set(None); key_state.chipmunk_track.set(None); key_state.information_track.set(None); key_state.queue_open.set(false); key_state.options_open.set(false); key_state.video_open.set(false); }
            },
            crate::native::WindowControls {}
            div { class: "app-body",
                Sidebar {}
                main { class: "main-wrap",
                    div {class:"content-drag",style:"pointer-events:auto",onmousedown:move|event|if event.trigger_button()==Some(dioxus::html::input_data::MouseButton::Primary){if let Some(window)=&window{window.drag();}}}
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
            crate::context_menu::ContextMenuHost {}
            crate::updates::UpdatePrompt {}
            if *importing.read(){div{class:"import-drop-zone",role:"status",aria_live:"polite",div{class:"import-drop-zone__card",div{class:"import-drop-zone__icon",Icon{name:"folder-input",size:28}Icon{name:"file-music",size:17}}div{class:"import-drop-zone__title","Añadir a tu biblioteca"}div{class:"import-drop-zone__hint","Suelta canciones, vídeos, carpetas o playlists M3U"}}}}
            if s.video_track.read().is_some() { crate::player::VideoPanel {} }
            if let Some(track) = s.chipmunk_track.read().clone() {
                crate::processing_ui::ChipmunkDialog { track, onclose: { let mut state=s.clone(); move |_| state.chipmunk_track.set(None) } }
            }
            if let Some(track) = s.information_track.read().clone() {
                crate::library_ui::InformationDialog { track, onclose: { let mut state = s.clone(); move |_| state.information_track.set(None) } }
            }
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
    let window = try_consume_context::<dioxus::desktop::DesktopContext>();
    let menu_state = s.clone();
    let mut menu_button = use_signal(|| None::<Rc<MountedData>>);
    rsx! {
        aside { class: if cfg!(target_os="macos"){"sidebar"}else{"sidebar sidebar--opaque"},
            div { class: "sidebar-drag", onmousedown: move |event| { if event.trigger_button() == Some(dioxus::html::input_data::MouseButton::Primary) { if let Some(window) = &window { window.drag(); } } },
                if cfg!(target_os="windows"){button{class:"sidebar-menu-button",r#type:"button",title:"Menú",aria_label:"Menú de la aplicación",onmounted:move|event|menu_button.set(Some(event.data())),onmousedown:|event|event.stop_propagation(),onclick:move|event|{
                    event.stop_propagation();let button=menu_button.peek().clone();let state=menu_state.clone();let menus=try_consume_context::<crate::context_menu::Menus>();
                    spawn(async move{if let(Some(button),Some(menus))=(button,menus){if let Ok(rect)=button.get_client_rect().await{menus.show(rect.min_x(),rect.max_y()+4.0,crate::native::application_items(&state));}}});
                },Icon{name:"menu"}}}
            }
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
            Icon { name: page.icon(),class:"sidebar-icon",stroke_width:if page==active{2.1}else{1.8} }
            span { class: "sidebar-item__label", "{page.label()}" }
            if page == Page::Downloads && badge > 0 { span { class: "sidebar-badge", "{badge}" } }
        }
    }
}

#[component]
pub fn Thumb(
    #[props(default = String::new())] src: String,
    #[props(default = "sm")] size: &'static str,
    #[props(default = false)] video: bool,
    #[props(default = None)] icon_size: Option<u32>,
    #[props(default)] children: Element,
) -> Element {
    let mut failed = use_signal(String::new);
    let failed_src = src.clone();
    let allowed = src.starts_with("https://")
        || src.starts_with("data:image/")
        || src.starts_with("/artwork/");
    rsx! { div { class: "thumb thumb--{size}",
        if allowed && *failed.read() != src {
            img { src, alt: "", loading: "lazy", draggable: false, onerror: move |_| failed.set(failed_src.clone()) }
        } else { div { class: "thumb-placeholder", Icon { name: if video { "video" } else { "music" }, size: icon_size.unwrap_or(if size == "square" { 26 } else { 14 }) } } }
        {children}
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
    #[props(default = false)] compact: bool,
    #[props(default)] children: Element,
) -> Element {
    rsx! { div { class: if compact { "empty-state empty-state--compact" } else { "empty-state" }, Icon { name: icon, class:"empty-state__icon",stroke_width:1.4,size: if compact { 24 } else { 30 } } if !title.is_empty(){span { class: "empty-state__title", "{title}" }}if !hint.is_empty(){span { class: "empty-state__hint", "{hint}" }} {children} } }
}

#[component]
pub fn MediaCard(track: Track, #[props(default = false)] home: bool) -> Element {
    let s = use_context::<AppState>();
    let active = s.current.read().as_ref().is_some_and(|t| t.id == track.id);
    let t = track.clone();
    let menu = track.clone();
    let play_state = s.clone();
    let context_state = s.clone();
    let drag_state = s.clone();
    let drag_track = track.clone();
    let mut dragging = use_signal(|| false);
    rsx! { button { class: "media-card", title: "{track.title}",draggable: !track.missing && !track.path.as_os_str().is_empty(),onmousedown:move |_|dragging.set(false),ondragstart:move|event|{event.prevent_default();event.stop_propagation();dragging.set(true);crate::native::begin_media_drag(&drag_state,&drag_track);},
        onclick: move |_| { if *dragging.peek(){return;}
            if active { play_state.toggle_play(); } else { play_state.play(t.clone()); } },
        oncontextmenu: move |event| crate::context_menu::show(event,if home{crate::context_menu::home_items(context_state.clone(),menu.clone())}else{crate::context_menu::track_items(context_state.clone(),menu.clone())}),
        div { class: "card-cover", Thumb { src: crate::app::artwork_src(&track), size: "square", video: track.media_type == "video" }
            span { class: "card-play", Icon { name: if active && *s.playing.read() { "pause" } else { "play" },size:15,fill:"currentColor",style:if active && *s.playing.read(){""}else{"margin-left:2px"} } }
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
    let s = use_context::<AppState>();
    let active = s
        .current
        .read()
        .as_ref()
        .is_some_and(|current| current.id == track.id);
    let click = track.clone();
    let menu = track.clone();
    let more = track.clone();
    let drag_track = track.clone();
    let row_state = s.clone();
    let context_state = s.clone();
    let more_state = s.clone();
    let drag_state = s.clone();
    let mut dragging = use_signal(|| false);
    rsx! {div{class:if active{"row row--active"}else{"row"},draggable:!track.missing&&!track.path.as_os_str().is_empty(),onmousedown:move |_|dragging.set(false),ondragstart:move|event|{event.prevent_default();event.stop_propagation();dragging.set(true);crate::native::begin_media_drag(&drag_state,&drag_track);},
        onclick:move |_|{if *dragging.peek(){return;}
            if active{row_state.toggle_play();}else{row_state.play(click.clone());}},
        oncontextmenu:move|event|crate::context_menu::show_track(event,context_state.clone(),menu.clone()),
        span{class:"row__index",if active && *s.playing.read(){Equalizer{}}else{"{index+1}"}}
        Thumb{src:crate::app::artwork_src(&track),video:track.media_type=="video",icon_size:Some(14)}
        div{class:"row__meta",div{class:if active{"row__title row__title--active"}else{"row__title"},"{track.title}"}div{class:"row__sub",if track.artist.is_empty(){"{track.filename}"}else{"{track.artist}"}}}
        span{class:"row__detail",if track.duration>0.0{"{time_label(track.duration)}"}else{"--:--"}}
        span{class:"row__detail",style:"width:56px;text-align:right","{size_label(track.size_bytes)}"}
        button{class:"btn-icon-sm",title:"Más opciones",aria_label:"Más opciones de {track.title}",onclick:move|event|crate::context_menu::show_track(event,more_state.clone(),more.clone()),Icon{name:"more",size:15}}
    }}
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::dioxus_core::{AttributeValue, ElementId, Mutation, Mutations};
    use std::cell::RefCell;

    type NavigationCapture = Rc<RefCell<Option<AppState>>>;

    fn navigation_harness() -> Element {
        let state = use_app_state(use_context::<Rc<Services>>());
        *use_context::<NavigationCapture>().borrow_mut() = Some(state.clone());
        use_context_provider(|| state.clone());
        // Like Application, the page host and Sidebar subscribe independently.
        // Rendering both catches a child whose props or signal become stale
        // while its parent already displays the newly selected page.
        let selected = *state.page.read();
        rsx! {
            Sidebar {}
            main { "data-selected-page": selected.label(), "{selected.label()}" }
        }
    }

    fn changed_attribute(mutations: &Mutations, id: ElementId, name: &str, expected: &str) -> bool {
        mutations.edits.iter().any(|mutation| {
            matches!(mutation, Mutation::SetAttribute { id: actual_id, name: actual_name,
                value: AttributeValue::Text(value), .. }
                if *actual_id == id && *actual_name == name && value == expected)
        })
    }

    #[tokio::test]
    async fn sidebar_click_updates_active_class_aria_and_selected_page_together() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let mut settings = services.core.settings().unwrap();
        settings.show_advanced_tools = true;
        services.core.save_settings(&settings).unwrap();
        let capture: NavigationCapture = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(navigation_harness);
        dom.insert_any_root_context(Box::new(services));
        dom.insert_any_root_context(Box::new(capture.clone()));
        let initial = dom.rebuild_to_vec();
        let mut labeled_buttons = std::collections::HashMap::new();
        let mut button = None;
        for mutation in &initial.edits {
            match mutation {
                Mutation::SetAttribute {
                    name: "aria-current",
                    id,
                    ..
                } => button = Some(*id),
                Mutation::CreateTextNode { value, .. } => {
                    if let Some(id) = button.take() {
                        labeled_buttons.insert(value.as_str(), id);
                    }
                }
                _ => {}
            }
        }
        assert_eq!(labeled_buttons.len(), 7);
        let pages = [
            Page::Home,
            Page::Search,
            Page::Library,
            Page::Playlists,
            Page::Stems,
            Page::Downloads,
            Page::Settings,
        ];
        let buttons = pages.map(|page| labeled_buttons[page.label()]);
        let mut previous = 0;
        for index in [2, 3, 1, 4, 5, 6, 0, 2] {
            dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
            let data = dioxus::html::PlatformEventData::new(Box::new(
                dioxus::html::SerializedMouseData::default(),
            ));
            dom.runtime()
                .handle_event("click", Event::new(Rc::new(data), true), buttons[index]);
            let mutations = dom.render_immediate_to_vec();
            let selected = pages[index];
            assert!(
                *capture.borrow().as_ref().unwrap().page.peek() == selected,
                "Clicked button {index}: expected {}, got {}",
                selected.label(),
                capture.borrow().as_ref().unwrap().page.peek().label()
            );
            // Inspect the real renderer mutations as well as SSR: a stale
            // WebView class must not pass merely because its VDOM is correct.
            assert!(changed_attribute(
                &mutations,
                buttons[previous],
                "class",
                "sidebar-item"
            ));
            assert!(changed_attribute(
                &mutations,
                buttons[previous],
                "aria-current",
                "false"
            ));
            assert!(changed_attribute(
                &mutations,
                buttons[index],
                "class",
                "sidebar-item active"
            ));
            assert!(changed_attribute(
                &mutations,
                buttons[index],
                "aria-current",
                "page"
            ));
            let html = dioxus_ssr::render(&dom);
            assert_eq!(html.matches("class=\"sidebar-item active\"").count(), 1);
            assert_eq!(html.matches("aria-current=\"page\"").count(), 1);
            assert!(html.contains(&format!("data-selected-page=\"{}\"", selected.label())));
            for page in pages {
                let button = html
                    .split("</button>")
                    .find(|part| part.contains(&format!(">{}</span>", page.label())))
                    .unwrap();
                assert_eq!(
                    button.contains("class=\"sidebar-item active\""),
                    page == selected
                );
                assert_eq!(button.contains("aria-current=\"page\""), page == selected);
            }
            previous = index;
        }
    }

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
            "Nada en reproducción",
        ] {
            assert!(html.contains(label), "missing UI label: {label}");
        }
        assert_eq!(html.contains("sidebar--opaque"), !cfg!(target_os = "macos"));
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
