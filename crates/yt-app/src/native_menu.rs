//! Shared application actions for the AppKit menu bar and Windows hamburger menu.
use super::*;
use crate::context_menu::Entry;
use dioxus::desktop::muda::{CheckMenuItem, IsMenuItem};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
};

thread_local! {
    static MENU: RefCell<Option<Menu>> = const { RefCell::new(None) };
    static ACTIONS: RefCell<HashMap<String, Action>> = RefCell::new(HashMap::new());
    static GENERATION: Cell<u64> = const { Cell::new(0) };
    static ZOOM: Cell<f64> = const { Cell::new(1.0) };
    #[cfg(target_os = "windows")]
    static WINDOW_HANDLE: Cell<isize> = const { Cell::new(0) };
}

#[cfg(target_os = "windows")]
pub(super) fn translate_message(message: *const std::ffi::c_void) -> bool {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn TranslateAcceleratorW(
            window: isize,
            accelerators: isize,
            message: *const std::ffi::c_void,
        ) -> i32;
    }
    let window = WINDOW_HANDLE.with(Cell::get);
    let accelerators = MENU
        .with(|slot| slot.borrow().as_ref().map(Menu::haccel))
        .unwrap_or(0);
    if window == 0 || accelerators == 0 || message.is_null() {
        return false;
    }
    // SAFETY: Tao supplies a live MSG for this callback. The UI thread owns
    // the HWND and the current Muda accelerator table for the same window.
    unsafe { TranslateAcceleratorW(window, accelerators, message) != 0 }
}

#[derive(Clone)]
enum Action {
    Named(&'static str),
    Navigate(Page),
    Settings(&'static str),
    Rate(f32),
    Edit(&'static str),
    Plugin(String, String),
}
struct Node {
    label: String,
    action: Option<Action>,
    enabled: bool,
    checked: Option<bool>,
    shortcut: Option<Accelerator>,
    children: Vec<Node>,
    role: bool,
    separator: bool,
}
impl Node {
    fn action(label: impl Into<String>, action: Action) -> Self {
        Self {
            label: label.into(),
            action: Some(action),
            enabled: true,
            checked: None,
            shortcut: None,
            children: vec![],
            role: false,
            separator: false,
        }
    }
    fn named(label: impl Into<String>, action: &'static str) -> Self {
        Self::action(label, Action::Named(action))
    }
    fn menu(label: &str, children: Vec<Self>) -> Self {
        Self {
            children,
            action: None,
            ..Self::named(label, "")
        }
    }
    fn separator() -> Self {
        Self {
            separator: true,
            ..Self::named("", "")
        }
    }
    fn enabled(mut self, value: bool) -> Self {
        self.enabled = value;
        self
    }
    fn checked(mut self, value: bool) -> Self {
        self.checked = Some(value);
        self
    }
    fn shortcut(mut self, code: Code, modifiers: Modifiers) -> Self {
        self.shortcut = Some(Accelerator::new(Some(modifiers), code));
        self
    }
    fn role(mut self) -> Self {
        self.role = true;
        self
    }
}

pub(super) fn capture_menu(menu: &Menu) {
    MENU.with(|slot| *slot.borrow_mut() = Some(menu.clone()));
}

fn model(state: &AppState) -> Vec<Node> {
    let settings = state.settings.peek().clone();
    let has_media = state.current.peek().is_some();
    let video = state
        .current
        .peek()
        .as_ref()
        .is_some_and(|track| track.media_type == "video");
    let mut items = Vec::new();
    let preferences =
        || Node::action("Ajustes…", Action::Settings("")).shortcut(Code::Comma, CMD_OR_CTRL);
    if cfg!(target_os = "macos") {
        items.push(Node::menu(
            "YT Ultimate",
            vec![
                Node::named("Acerca de YT Ultimate", "about").role(),
                preferences(),
                Node::separator(),
                Node::named("Servicios", "services").role(),
                Node::separator(),
                Node::named("Ocultar YT Ultimate", "hide").role(),
                Node::named("Ocultar otros", "hide-others").role(),
                Node::named("Mostrar todo", "show-all").role(),
                Node::separator(),
                Node::named("Salir de YT Ultimate", "quit").shortcut(Code::KeyQ, CMD_OR_CTRL),
            ],
        ));
    }
    let manager = if cfg!(target_os = "macos") {
        "Finder"
    } else {
        "Explorador"
    };
    let mut file = vec![
        Node::named("Importar archivos…", "import").shortcut(Code::KeyO, CMD_OR_CTRL),
        Node::named("Importar carpeta…", "import-folder")
            .shortcut(Code::KeyO, CMD_OR_CTRL | Modifiers::SHIFT),
    ];
    #[cfg(target_os = "macos")]
    file.push(Node::menu("Abrir recientes", vec![]));
    file.extend([
        Node::separator(),
        Node::named(format!("Mostrar biblioteca en {manager}"), "library-folder")
            .shortcut(Code::KeyL, CMD_OR_CTRL | Modifiers::SHIFT),
        Node::separator(),
    ]);
    if cfg!(target_os = "macos") {
        file.push(Node::named("Cerrar ventana", "close").shortcut(Code::KeyW, CMD_OR_CTRL));
    } else {
        file.extend([
            preferences(),
            Node::separator(),
            Node::named("Salir", "quit"),
        ]);
    }
    items.push(Node::menu("Archivo", file));
    items.push(Node::menu(
        "Edición",
        vec![
            Node::action("Deshacer", Action::Edit("undo")).role(),
            Node::action("Rehacer", Action::Edit("redo")).role(),
            Node::separator(),
            Node::action("Cortar", Action::Edit("cut")).role(),
            Node::action("Copiar", Action::Edit("copy")).role(),
            Node::action("Pegar", Action::Edit("paste")).role(),
            Node::action("Seleccionar todo", Action::Edit("selectAll")).role(),
        ],
    ));
    let mut playback = vec![
        Node::named(
            if *state.playing.peek() {
                "Pausar"
            } else {
                "Reproducir"
            },
            "play-pause",
        )
        .enabled(has_media),
        Node::named("Anterior", "previous")
            .enabled(has_media)
            .shortcut(Code::ArrowLeft, CMD_OR_CTRL | Modifiers::ALT),
        Node::named("Siguiente", "next")
            .enabled(has_media)
            .shortcut(Code::ArrowRight, CMD_OR_CTRL | Modifiers::ALT),
        Node::separator(),
        Node::named("Retroceder 10 segundos", "backward")
            .enabled(has_media)
            .shortcut(Code::ArrowLeft, CMD_OR_CTRL | Modifiers::SHIFT),
        Node::named("Adelantar 10 segundos", "forward")
            .enabled(has_media)
            .shortcut(Code::ArrowRight, CMD_OR_CTRL | Modifiers::SHIFT),
        Node::separator(),
        Node::named("Aleatorio", "shuffle")
            .checked(*state.shuffle.peek())
            .shortcut(Code::KeyS, CMD_OR_CTRL | Modifiers::ALT),
        Node::named(
            if *state.repeat.peek() == 2 {
                "Repetir una"
            } else {
                "Repetir"
            },
            "repeat",
        )
        .checked(*state.repeat.peek() != 0)
        .shortcut(Code::KeyR, CMD_OR_CTRL | Modifiers::ALT),
        Node::menu(
            "Velocidad",
            [0.75, 1.0, 1.25, 1.5, 2.0]
                .into_iter()
                .map(|rate| {
                    Node::action(format!("{rate}×"), Action::Rate(rate))
                        .checked((*state.rate.peek() - rate).abs() < 0.001)
                })
                .collect(),
        ),
        Node::separator(),
        Node::named("Silenciar", "mute")
            .enabled(has_media)
            .shortcut(Code::KeyM, CMD_OR_CTRL | Modifiers::SHIFT),
    ];
    if video {
        playback.push(Node::named("Mostrar vídeo", "show-video"));
    }
    items.push(Node::menu("Reproducción", playback));
    let mut view: Vec<Node> = [
        (Page::Home, Code::Digit1),
        (Page::Search, Code::Digit2),
        (Page::Library, Code::Digit3),
        (Page::Playlists, Code::Digit4),
        (Page::Stems, Code::Digit5),
        (Page::Downloads, Code::Digit6),
        (Page::Settings, Code::Digit7),
    ]
    .into_iter()
    .filter(|(page, _)| *page != Page::Stems || settings.show_advanced_tools)
    .map(|(page, key)| {
        Node::action(page.label(), Action::Navigate(page)).shortcut(key, CMD_OR_CTRL)
    })
    .collect();
    view.extend([
        Node::action("Buscar…", Action::Navigate(Page::Search)).shortcut(Code::KeyF, CMD_OR_CTRL),
        Node::separator(),
        Node::named("Cola de reproducción", "queue").shortcut(Code::Digit0, CMD_OR_CTRL),
        Node::separator(),
        Node::named("Tamaño real", "zoom-reset")
            .shortcut(Code::Digit0, CMD_OR_CTRL | Modifiers::ALT),
        Node::named("Acercar", "zoom-in").shortcut(Code::Equal, CMD_OR_CTRL),
        Node::named("Alejar", "zoom-out").shortcut(Code::Minus, CMD_OR_CTRL),
        Node::separator(),
        Node::named("Pantalla completa", "fullscreen").shortcut(
            if cfg!(target_os = "macos") {
                Code::KeyF
            } else {
                Code::F11
            },
            if cfg!(target_os = "macos") {
                CMD_OR_CTRL | Modifiers::CONTROL
            } else {
                Modifiers::empty()
            },
        ),
    ]);
    items.push(Node::menu("Visualización", view));
    let window = if cfg!(target_os = "macos") {
        vec![
            Node::named("Minimizar", "minimize").role(),
            Node::named("Zoom", "maximize").role(),
            Node::separator(),
            Node::named("Traer todo al frente", "front").role(),
        ]
    } else {
        vec![
            Node::named("Minimizar", "minimize").role(),
            Node::named("Cerrar ventana", "close").shortcut(Code::KeyW, CMD_OR_CTRL),
        ]
    };
    items.push(Node::menu("Ventana", window));
    if settings.developer_mode {
        let commands = state
            .services
            .plugins
            .as_ref()
            .ok()
            .and_then(|host| host.plugins().ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|plugin| plugin.compatible && plugin.enabled)
            .flat_map(|plugin| {
                plugin.commands.into_iter().map(move |command| {
                    Node::action(
                        format!("{} · {}", plugin.name, command.label),
                        Action::Plugin(plugin.id.clone(), command.id),
                    )
                })
            })
            .collect::<Vec<_>>();
        items.push(Node::menu(
            "Desarrollo",
            vec![
                Node::named("Abrir carpeta de plugins", "plugins-folder")
                    .shortcut(Code::KeyP, CMD_OR_CTRL | Modifiers::ALT),
                Node::named("Recargar plugins", "reload-plugins")
                    .shortcut(Code::KeyR, CMD_OR_CTRL | Modifiers::ALT | Modifiers::SHIFT),
                Node::action("Crear plugin…", Action::Settings("plugin-scaffold")),
                Node::separator(),
                Node::menu(
                    "Comandos de plugins",
                    if commands.is_empty() {
                        vec![Node::named("No hay comandos registrados", "").enabled(false)]
                    } else {
                        commands
                    },
                ),
                Node::separator(),
                Node::named("Abrir logs", "logs"),
                Node::named("Documentación del SDK", "sdk"),
                Node::separator(),
                Node::named("Herramientas de desarrollo", "devtools")
                    .shortcut(Code::KeyI, CMD_OR_CTRL | Modifiers::ALT),
                Node::named("Recargar interfaz", "reload-ui")
                    .shortcut(Code::KeyR, CMD_OR_CTRL | Modifiers::ALT),
            ],
        ));
    }
    let mut help = vec![
        Node::named("Guía de YT Ultimate", "help"),
        Node::action("Reportar un problema…", Action::Settings("diagnostics")),
    ];
    if !cfg!(target_os = "macos") {
        help.extend([
            Node::separator(),
            Node::action("Buscar actualizaciones…", Action::Settings("updates")),
            Node::named("Acerca de YT Ultimate", "about").role(),
        ]);
    }
    items.push(Node::menu("Ayuda", help));
    items
}

pub(super) fn application_items(state: &AppState) -> Vec<Entry> {
    fn convert(state: &AppState, node: Node) -> Entry {
        if node.separator {
            return Entry::separator();
        }
        if !node.children.is_empty() {
            return Entry::submenu(
                node.label,
                node.children
                    .into_iter()
                    .map(|node| convert(state, node))
                    .collect(),
            );
        }
        let owned = state.clone();
        let mut entry = Entry::action(node.label, move || {
            if let Some(action) = &node.action {
                dispatch(&owned, action.clone());
            }
        })
        .enabled(node.enabled);
        entry.checked = node.checked;
        entry
    }
    model(state)
        .into_iter()
        .map(|node| convert(state, node))
        .collect()
}

fn predefined(action: &Action) -> Option<PredefinedMenuItem> {
    Some(match action {
        Action::Edit("undo") => PredefinedMenuItem::undo(Some("Deshacer")),
        Action::Edit("redo") => PredefinedMenuItem::redo(Some("Rehacer")),
        Action::Edit("cut") => PredefinedMenuItem::cut(Some("Cortar")),
        Action::Edit("copy") => PredefinedMenuItem::copy(Some("Copiar")),
        Action::Edit("paste") => PredefinedMenuItem::paste(Some("Pegar")),
        Action::Edit("selectAll") => PredefinedMenuItem::select_all(Some("Seleccionar todo")),
        Action::Named("minimize") => PredefinedMenuItem::minimize(Some("Minimizar")),
        Action::Named("maximize") => PredefinedMenuItem::maximize(Some("Zoom")),
        Action::Named("hide") => PredefinedMenuItem::hide(Some("Ocultar YT Ultimate")),
        Action::Named("hide-others") => PredefinedMenuItem::hide_others(Some("Ocultar otros")),
        Action::Named("show-all") => PredefinedMenuItem::show_all(Some("Mostrar todo")),
        Action::Named("front") => {
            PredefinedMenuItem::bring_all_to_front(Some("Traer todo al frente"))
        }
        Action::Named("services") => PredefinedMenuItem::services(Some("Servicios")),
        Action::Named("about") => PredefinedMenuItem::about(
            Some("Acerca de YT Ultimate"),
            Some(AboutMetadata {
                name: Some("YT Ultimate".into()),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                ..Default::default()
            }),
        ),
        _ => return None,
    })
}

fn rebuild(state: &AppState) -> Result<(), dioxus::desktop::muda::Error> {
    let Some(menu) = MENU.with(|slot| slot.borrow().clone()) else {
        return Ok(());
    };
    let generation = GENERATION.with(|value| {
        let next = value.get().wrapping_add(1);
        value.set(next);
        next
    });
    let mut actions = HashMap::new();
    fn append(
        nodes: Vec<Node>,
        prefix: &str,
        actions: &mut HashMap<String, Action>,
        add: &dyn Fn(&dyn IsMenuItem) -> Result<(), dioxus::desktop::muda::Error>,
    ) -> Result<(), dioxus::desktop::muda::Error> {
        for (index, node) in nodes.into_iter().enumerate() {
            let id = format!("{prefix}-{index}");
            if node.separator {
                add(&PredefinedMenuItem::separator())?;
                continue;
            }
            if node.action.is_none() {
                let sub = Submenu::new(&node.label, node.enabled);
                append(node.children, &id, actions, &|item| sub.append(item))?;
                #[cfg(target_os = "macos")]
                match node.label.as_str() {
                    "Abrir recientes" => {
                        RECENT_MENU.with(|slot| *slot.borrow_mut() = Some(sub.clone()));
                        refresh_recent_menu();
                    }
                    "Ventana" => sub.set_as_windows_menu_for_nsapp(),
                    "Ayuda" => sub.set_as_help_menu_for_nsapp(),
                    _ => {}
                }
                add(&sub)?;
            } else if let Some(item) = node
                .action
                .as_ref()
                .filter(|_| node.role)
                .and_then(predefined)
            {
                add(&item)?;
            } else {
                if let Some(action) = node.action {
                    actions.insert(id.clone(), action);
                }
                if let Some(checked) = node.checked {
                    add(&CheckMenuItem::with_id(
                        id,
                        &node.label,
                        node.enabled,
                        checked,
                        node.shortcut,
                    ))?;
                } else {
                    add(&MenuItem::with_id(
                        id,
                        &node.label,
                        node.enabled,
                        node.shortcut,
                    ))?;
                }
            }
        }
        Ok(())
    }
    while menu.remove_at(0).is_some() {}
    append(
        model(state),
        &format!("ytu-app-{generation}"),
        &mut actions,
        &|item| menu.append(item),
    )?;
    ACTIONS.with(|slot| *slot.borrow_mut() = actions);
    Ok(())
}

pub(super) fn use_application_menu(state: AppState) {
    let window = use_window();
    #[cfg(target_os = "windows")]
    use_hook({
        let window = window.clone();
        move || {
            use dioxus::desktop::tao::platform::windows::WindowExtWindows;
            WINDOW_HANDLE.with(|handle| handle.set(window.window.hwnd()));
            MENU.with(|slot| {
                if let Some(menu) = slot.borrow().as_ref() {
                    // SAFETY: the live Tao window owns this HWND on the UI thread.
                    let _ = unsafe { menu.hide_for_hwnd(window.window.hwnd()) };
                }
            });
        }
    });
    let previous = use_hook(|| std::rc::Rc::new(RefCell::new(None)));
    use_effect(move || {
        let settings = state.settings.read();
        let current = state.current.read();
        let snapshot = (
            settings.developer_mode,
            settings.show_advanced_tools,
            current.is_some(),
            current
                .as_ref()
                .is_some_and(|track| track.media_type == "video"),
            *state.playing.read(),
            *state.shuffle.read(),
            *state.repeat.read(),
            state.rate.read().to_bits(),
            *state.plugins_revision.read(),
        );
        drop(current);
        drop(settings);
        if previous.borrow().as_ref() == Some(&snapshot) {
            return;
        }
        *previous.borrow_mut() = Some(snapshot);
        if !state.settings.peek().developer_mode && window.webview.is_devtools_open() {
            window.webview.close_devtools();
        }
        if let Err(error) = rebuild(&state) {
            state.notify(format!("No se pudo actualizar el menú: {error}"));
        }
    });
}

pub(super) fn handle_event(state: &AppState, id: &str) -> bool {
    if !id.starts_with("ytu-app-") {
        return false;
    }
    let action = ACTIONS.with(|actions| actions.borrow().get(id).cloned());
    if let Some(action) = action {
        dispatch(state, action);
    }
    true
}

fn dispatch(state: &AppState, action: Action) {
    let mut state = state.clone();
    let window = dioxus::desktop::window();
    match action {
        Action::Navigate(page) => {
            state.settings_section.set(String::new());
            state.page.set(page);
            if page == Page::Search {
                focus_search();
            }
        }
        Action::Settings(section) => {
            state.settings_section.set(section.into());
            state.page.set(Page::Settings);
        }
        Action::Rate(rate) => state.set_rate(rate),
        Action::Edit(command) => crate::context_menu::edit_current(&state, command),
        Action::Plugin(plugin, command) => {
            if !state.settings.peek().developer_mode {
                return;
            }
            let Ok(host) = state.services.plugins.clone() else {
                return;
            };
            dioxus::core::spawn_forever(async move {
                let result = tokio::task::spawn_blocking(move || {
                    host.execute(&plugin, &command, serde_json::json!({}))
                })
                .await;
                state.refresh();
                state.plugins_revision += 1;
                match result {
                    Ok(Ok(value)) => state.notify(value.to_string()),
                    Ok(Err(error)) => state.notify(error.to_string()),
                    Err(error) => state.notify(error.to_string()),
                }
            });
        }
        Action::Named(name) => match name {
            "import" => state.import_files(false),
            "import-folder" => state.import_files(true),
            "library-folder" => open_folder(&state, &state.services.library_dir),
            "play-pause" => state.toggle_play(),
            "previous" => state.previous(),
            "next" => state.next(false),
            "backward" => {
                let position = *state.position.peek();
                state.seek((position - 10.0).max(0.0));
            }
            "forward" => {
                let position = *state.position.peek();
                state.seek(position + 10.0);
            }
            "shuffle" => state.toggle_shuffle(),
            "repeat" => state.cycle_repeat(),
            "mute" => state.toggle_mute(),
            "queue" => state.queue_open.toggle(),
            "show-video" => state.video_open.set(true),
            "fullscreen" => window.set_fullscreen(window.window.fullscreen().is_none()),
            "minimize" => window.window.set_minimized(true),
            "maximize" => window.window.set_maximized(!window.window.is_maximized()),
            "close" | "quit" => request_close(&state),
            "zoom-reset" | "zoom-in" | "zoom-out" => ZOOM.with(|zoom| {
                let level = match name {
                    "zoom-in" => (zoom.get() * 1.1).min(3.0),
                    "zoom-out" => (zoom.get() / 1.1).max(0.5),
                    _ => 1.0,
                };
                zoom.set(level);
                window.set_zoom_level(level);
            }),
            "plugins-folder" => {
                if let Ok(host) = &state.services.plugins {
                    open_folder(&state, host.directory());
                }
            }
            "reload-plugins" => {
                state.plugins_revision += 1;
                state.notify("Plugins recargados");
            }
            "devtools" if state.settings.peek().developer_mode => {
                if window.webview.is_devtools_open() {
                    window.webview.close_devtools();
                } else {
                    window.webview.open_devtools();
                }
            }
            "reload-ui" if state.settings.peek().developer_mode => {
                state.persist_playback();
                if let Err(error) = window.webview.reload() {
                    state.notify(error.to_string());
                }
            }
            "help" => show_document(&state, "GUIA.md", include_str!("../../../README.md")),
            "sdk" => show_document(
                &state,
                "PLUGIN_SDK.md",
                include_str!("../../yt-plugins/README.md"),
            ),
            "logs" => open_logs(&state),
            "about" => {
                run_native_action(&state, move || {
                    rfd::MessageDialog::new()
                        .set_title("YT Ultimate")
                        .set_description(format!("YT Ultimate {}", env!("CARGO_PKG_VERSION")))
                        .show();
                    Ok(())
                });
            }
            _ => {}
        },
    }
}

fn show_document(state: &AppState, name: &str, contents: &'static str) {
    let directory = state.services.profile.join("Documentation");
    let path = directory.join(name);
    run_native_action(state, move || {
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        std::fs::write(&path, contents).map_err(|error| error.to_string())?;
        open_native(path.as_os_str())
    });
}
fn open_logs(state: &AppState) {
    let directory = state.services.profile.join("Logs");
    let report = format!(
        "YT Ultimate {}\nOS: {} {}\nLibrary: {}\nProfile: {}\nAudio: {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        state.services.library_dir.display(),
        state.services.profile.display(),
        match &state.services.audio {
            Ok(_) => "available",
            Err(error) => error,
        }
    );
    let state = state.clone();
    dioxus::core::spawn_forever(async move {
        let path = directory.clone();
        match tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&path)?;
            std::fs::write(path.join("diagnostics.txt"), report)
        })
        .await
        {
            Ok(Ok(())) => open_folder(&state, &directory),
            Ok(Err(error)) => state.notify(error.to_string()),
            Err(error) => state.notify(error.to_string()),
        }
    });
}

fn focus_search() {
    document::eval("requestAnimationFrame(()=>{const input=document.getElementById('ytu-search');if(input){input.focus();input.select();}});");
}
