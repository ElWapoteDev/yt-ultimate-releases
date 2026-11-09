use crate::state::{AppState, Page};
use dioxus::{
    desktop::{
        muda::{
            accelerator::{Accelerator, Code, Modifiers, CMD_OR_CTRL},
            AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu,
        },
        tao::event::{Event, WindowEvent},
        use_muda_event_handler, use_window, use_wry_event_handler,
    },
    prelude::*,
};
use std::{
    path::PathBuf,
    process::Command,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

#[path = "native_menu.rs"]
mod application;
#[path = "native_log.rs"]
mod logging;
#[path = "native_media.rs"]
mod media_controls;
#[path = "native_window.rs"]
mod window_controls;
pub(crate) use logging::record_log;
pub(crate) use window_controls::WindowControls;

pub(crate) fn application_items(state: &AppState) -> Vec<crate::context_menu::Entry> {
    application::application_items(state)
}

pub(crate) fn configure(config: dioxus::desktop::Config) -> dioxus::desktop::Config {
    #[cfg(target_os = "windows")]
    {
        use dioxus::desktop::tao::{
            event_loop::EventLoopBuilder, platform::windows::EventLoopBuilderExtWindows,
        };
        let mut event_loop = EventLoopBuilder::with_user_event();
        event_loop.with_msg_hook(application::translate_message);
        config.with_event_loop(event_loop.build())
    }
    #[cfg(not(target_os = "windows"))]
    config
}

pub(crate) fn read_clipboard_text() -> Result<String, String> {
    arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.get_text())
        .map_err(|error| error.to_string())
}

pub(crate) fn write_clipboard_text(text: &str) -> Result<(), String> {
    if text.len() > 16 * 1024 * 1024 {
        return Err("El texto supera el límite de 16 MiB del portapapeles.".into());
    }
    arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.set_text(text))
        .map_err(|error| error.to_string())
}

// macOS can deliver document events before the first Dioxus component mounts.
// Keep those paths until the application's importer is available.
static OPEN_FILES: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

#[cfg(target_os = "macos")]
thread_local! {
    static RECENT_MENU: std::cell::RefCell<Option<Submenu>> = const { std::cell::RefCell::new(None) };
    static RECENT_PATHS: std::cell::RefCell<Vec<(String, PathBuf)>> = const { std::cell::RefCell::new(Vec::new()) };
    static RECENT_GENERATION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Call before creating a native window so taskbar grouping and recent files
/// use the same identity as the original Electron application and shortcuts.
pub(crate) fn initialize() {
    #[cfg(target_os = "windows")]
    {
        let identity = "com.ytultimate.desktop\0"
            .encode_utf16()
            .collect::<Vec<_>>();
        // SAFETY: identity is a live, nul-terminated UTF-16 string.
        unsafe {
            SetCurrentProcessExplicitAppUserModelID(identity.as_ptr());
        }
    }
}

#[cfg(target_os = "windows")]
#[link(name = "shell32")]
unsafe extern "system" {
    fn SetCurrentProcessExplicitAppUserModelID(app_id: *const u16) -> i32;
    fn SHAddToRecentDocs(flags: u32, item: *const std::ffi::c_void);
}

/// Register a local file with the OS recent documents list. Call on the UI
/// thread after a successful import/open; missing files are never registered.
pub(crate) fn record_recent(path: &std::path::Path) {
    let Ok(path) = path.canonicalize() else {
        return;
    };
    if !path.is_file() {
        return;
    }
    #[cfg(target_os = "macos")]
    if let (Some(main), Some(url)) = (
        objc2::MainThreadMarker::new(),
        objc2_foundation::NSURL::from_file_path(&path),
    ) {
        objc2_app_kit::NSDocumentController::sharedDocumentController(main)
            .noteNewRecentDocumentURL(&url);
        refresh_recent_menu();
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        let path = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        // SHARD_PATHW; Windows associates the path with registered handlers.
        // SAFETY: path is a live, nul-terminated UTF-16 string.
        unsafe {
            SHAddToRecentDocs(0x3, path.as_ptr().cast());
        }
    }
}

#[derive(Default)]
struct OutgoingMediaDrags {
    generations: Mutex<(u64, Option<u64>)>,
}

impl OutgoingMediaDrags {
    fn begin(self: &Arc<Self>) -> OutgoingMediaDrag {
        let mut generations = self
            .generations
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        generations.0 = generations.0.wrapping_add(1);
        let generation = generations.0;
        generations.1 = Some(generation);
        OutgoingMediaDrag {
            owner: self.clone(),
            generation,
        }
    }

    fn active(&self) -> bool {
        self.generations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .1
            .is_some()
    }
}

#[derive(Clone)]
struct OutgoingMediaDrag {
    owner: Arc<OutgoingMediaDrags>,
    generation: u64,
}

impl OutgoingMediaDrag {
    fn finish(&self) {
        let mut generations = self
            .owner
            .generations
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if generations.1 == Some(self.generation) {
            generations.1 = None;
        }
    }

    fn browser_script(&self, active: bool) -> String {
        if active {
            format!(
                "document.documentElement.dataset.ytuOutgoingDrag='{}';window.dispatchEvent(new Event('ytu-outgoing-drag'));",
                self.generation
            )
        } else {
            format!(
                "if(document.documentElement.dataset.ytuOutgoingDrag==='{}'){{delete document.documentElement.dataset.ytuOutgoingDrag;window.dispatchEvent(new Event('ytu-outgoing-drag'));}}",
                self.generation
            )
        }
    }
}

static OUTGOING_MEDIA_DRAGS: LazyLock<Arc<OutgoingMediaDrags>> =
    LazyLock::new(|| Arc::new(OutgoingMediaDrags::default()));

pub(crate) fn outgoing_media_drag_active() -> bool {
    OUTGOING_MEDIA_DRAGS.active()
}

/// Windows delivers queued WebView events after DoDragDrop returns. Keep the
/// Rust guard until the browser has acknowledged the end-of-drag script, so a
/// delayed self-drop cannot become a fresh external import.
async fn finish_outgoing_drag(
    session: OutgoingMediaDrag,
    ended: tokio::sync::oneshot::Receiver<()>,
    browser_delivered: impl std::future::Future<Output = ()>,
) {
    let _ = ended.await;
    browser_delivered.await;
    session.finish();
}

/// Begin a native copy drag from an explicit file handle, not an HTML drag.
/// Call synchronously from the primary mouse-down handler after preventing
/// the browser default and stopping propagation (playlist reordering is separate).
pub(crate) fn begin_media_drag(state: &AppState, track: &yt_core::Track) {
    let path = match track.path.canonicalize() {
        Ok(path) if path.is_file() => path,
        _ => {
            state.notify("El archivo ya no está disponible para arrastrarlo.");
            return;
        }
    };
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let session = OUTGOING_MEDIA_DRAGS.begin();
        document::eval(&session.browser_script(true));
        let (ended, receiver) = tokio::sync::oneshot::channel();
        let ended = Mutex::new(Some(ended));
        let completion = session.clone();
        let browser_session = session.clone();
        dioxus::core::spawn_forever(finish_outgoing_drag(completion, receiver, async move {
            let _ = document::eval(&browser_session.browser_script(false)).await;
        }));
        let window = dioxus::desktop::window();
        let result = drag::start_drag(
            window.window.as_ref(),
            drag::DragItem::Files(vec![path]),
            drag::Image::Raw(include_bytes!("../../../packaging/icons/drag-icon.png").to_vec()),
            move |_, _| {
                if let Some(ended) = ended
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                {
                    let _ = ended.send(());
                }
            },
            drag::Options {
                mode: drag::DragMode::Copy,
                ..Default::default()
            },
        );
        if let Err(error) = result {
            session.finish();
            document::eval(&session.browser_script(false));
            state.notify(format!("No se pudo iniciar el arrastre: {error}"));
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = path;
        state.notify("El arrastre nativo está disponible en Windows y Mac.");
    }
}

#[cfg(target_os = "macos")]
fn refresh_recent_menu() {
    let Some(main) = objc2::MainThreadMarker::new() else {
        return;
    };
    let controller = objc2_app_kit::NSDocumentController::sharedDocumentController(main);
    let paths = controller
        .recentDocumentURLs()
        .iter()
        .filter_map(|url| url.to_file_path())
        .take(15)
        .collect::<Vec<_>>();
    let generation = RECENT_GENERATION.with(|value| {
        let next = value.get().wrapping_add(1);
        value.set(next);
        next
    });
    RECENT_MENU.with(|slot| {
        let slot = slot.borrow();
        let Some(menu) = slot.as_ref() else {
            return;
        };
        while menu.remove_at(0).is_some() {}
        for (index, path) in paths.iter().enumerate() {
            let label = path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy();
            let _ = menu.append(&MenuItem::with_id(
                format!("ytu-recent-{generation}-{index}"),
                label,
                path.is_file(),
                None,
            ));
        }
        if paths.is_empty() {
            let _ = menu.append(&MenuItem::new("Sin documentos recientes", false, None));
        }
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::with_id(
            "ytu-clear-recent",
            "Borrar recientes",
            !paths.is_empty(),
            None,
        ));
    });
    RECENT_PATHS.with(|value| {
        *value.borrow_mut() = paths
            .into_iter()
            .enumerate()
            .map(|(index, path)| (format!("ytu-recent-{generation}-{index}"), path))
            .collect();
    });
}

pub fn capture_open_events<T>(event: &Event<T>) {
    if let Event::Opened { urls } = event {
        if let Ok(mut pending) = OPEN_FILES.lock() {
            for path in urls.iter().filter_map(|url| url.to_file_path().ok()) {
                if !pending.contains(&path) {
                    pending.push(path);
                }
            }
        }
    }
}

pub fn menu() -> Menu {
    let menu = Menu::new();
    let about = PredefinedMenuItem::about(
        Some("Acerca de YT Ultimate"),
        Some(AboutMetadata {
            name: Some("YT Ultimate".into()),
            version: Some(env!("CARGO_PKG_VERSION").into()),
            comments: Some("Vista previa de la migración Rust".into()),
            ..Default::default()
        }),
    );
    let settings = item("settings", "Ajustes…", Code::Comma, false);
    let quit = item("quit", "Salir de YT Ultimate", Code::KeyQ, false);
    if cfg!(target_os = "macos") {
        let app = Submenu::new("YT Ultimate", true);
        app.append_items(&[
            &about,
            &PredefinedMenuItem::separator(),
            &settings,
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::services(Some("Servicios")),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(Some("Ocultar YT Ultimate")),
            &PredefinedMenuItem::hide_others(Some("Ocultar otros")),
            &PredefinedMenuItem::show_all(Some("Mostrar todo")),
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .expect("valid application menu");
        menu.append(&app).expect("application menu can be appended");
    }
    let file = Submenu::new("Archivo", true);
    file.append_items(&[
        &item("import", "Importar archivos…", Code::KeyO, false),
        &item("import-folder", "Importar carpeta…", Code::KeyO, true),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(
            "ytu-library-folder",
            "Mostrar carpeta de biblioteca",
            true,
            None,
        ),
        &PredefinedMenuItem::separator(),
        &item("close", "Cerrar ventana", Code::KeyW, false),
    ])
    .expect("valid file menu");
    #[cfg(target_os = "macos")]
    {
        let recent = Submenu::new("Abrir recientes", true);
        file.insert(&recent, 2)
            .expect("recent submenu can be inserted");
        RECENT_MENU.with(|slot| *slot.borrow_mut() = Some(recent));
        refresh_recent_menu();
    }
    if !cfg!(target_os = "macos") {
        file.append_items(&[&PredefinedMenuItem::separator(), &settings, &quit])
            .expect("valid Windows file menu");
    }
    let edit = Submenu::new("Edición", true);
    edit.append_items(&[
        &PredefinedMenuItem::undo(Some("Deshacer")),
        &PredefinedMenuItem::redo(Some("Rehacer")),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::cut(Some("Cortar")),
        &PredefinedMenuItem::copy(Some("Copiar")),
        &PredefinedMenuItem::paste(Some("Pegar")),
        &PredefinedMenuItem::select_all(Some("Seleccionar todo")),
    ])
    .expect("valid edit menu");
    let playback = Submenu::new("Reproducción", true);
    playback
        .append_items(&[
            &MenuItem::with_id("ytu-play-pause", "Reproducir / Pausar", true, None),
            &item("previous", "Anterior", Code::ArrowLeft, false),
            &item("next", "Siguiente", Code::ArrowRight, false),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id("ytu-backward", "Retroceder 10 segundos", true, None),
            &MenuItem::with_id("ytu-forward", "Avanzar 10 segundos", true, None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id("ytu-queue", "Mostrar / ocultar cola", true, None),
        ])
        .expect("valid playback menu");
    let window = Submenu::new("Ventana", true);
    window
        .append_items(&[
            &PredefinedMenuItem::minimize(Some("Minimizar")),
            &PredefinedMenuItem::maximize(Some("Maximizar")),
            &MenuItem::with_id("ytu-fullscreen", "Alternar pantalla completa", true, None),
            &PredefinedMenuItem::separator(),
            &item("search", "Buscar", Code::KeyF, false),
        ])
        .expect("valid window menu");
    let help = Submenu::new("Ayuda", true);
    if !cfg!(target_os = "macos") {
        help.append(&about).expect("valid Windows about menu");
    }
    help.append_items(&[&MenuItem::with_id(
        "ytu-profile-folder",
        "Mostrar carpeta de datos",
        true,
        None,
    )])
    .expect("valid help menu");
    menu.append_items(&[&file, &edit, &playback, &window, &help])
        .expect("valid menu bar");
    #[cfg(target_os = "macos")]
    {
        window.set_as_windows_menu_for_nsapp();
        help.set_as_help_menu_for_nsapp();
    }
    application::capture_menu(&menu);
    menu
}

fn item(id: &str, label: &str, code: Code, shift: bool) -> MenuItem {
    let modifiers = if shift {
        CMD_OR_CTRL | Modifiers::SHIFT
    } else {
        CMD_OR_CTRL
    };
    MenuItem::with_id(
        format!("ytu-{id}"),
        label,
        true,
        Some(Accelerator::new(Some(modifiers), code)),
    )
}

async fn import_open_requests<F, I>(
    mut requests: tokio::sync::mpsc::UnboundedReceiver<Vec<PathBuf>>,
    mut import: F,
) where
    F: FnMut(Vec<PathBuf>) -> I,
    I: std::future::Future<Output = ()>,
{
    while let Some(paths) = requests.recv().await {
        import(paths).await;
    }
}

pub fn use_native(mut state: AppState) {
    application::use_application_menu(state.clone());
    window_controls::use_window_frame(&state);
    media_controls::use_media_controls(state.clone());
    use_hook({
        let state = state.clone();
        move || {
            record_log(
                &state.services.profile,
                &format!(
                    "Started YT Ultimate {} on {} {}",
                    env!("CARGO_PKG_VERSION"),
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ),
            )
        }
    });
    let window = use_window();
    #[cfg(target_os = "macos")]
    use_hook({
        let window = window.clone();
        let state = state.clone();
        move || {
            if let Err(error) = window_vibrancy::apply_vibrancy(
                window.window.as_ref(),
                window_vibrancy::NSVisualEffectMaterial::Sidebar,
                Some(window_vibrancy::NSVisualEffectState::Active),
                None,
            ) {
                record_log(
                    &state.services.profile,
                    &format!("Window vibrancy: {error}"),
                );
            }
        }
    });
    let native_window = window.clone();
    let close_state = state.clone();
    use_muda_event_handler(move |event| {
        if application::handle_event(&state, &event.id.0) {
            return;
        }
        match event.id.0.as_str() {
            "ytu-import" => state.import_files(false),
            "ytu-import-folder" => state.import_files(true),
            "ytu-settings" => state.page.set(Page::Settings),
            "ytu-search" => state.page.set(Page::Search),
            "ytu-play-pause" => state.toggle_play(),
            "ytu-previous" => state.previous(),
            "ytu-next" => state.next(false),
            "ytu-backward" => {
                let position = *state.position.peek();
                state.seek((position - 10.0).max(0.0));
            }
            "ytu-forward" => {
                let position = *state.position.peek();
                state.seek(position + 10.0);
            }
            "ytu-queue" => state.queue_open.toggle(),
            "ytu-fullscreen" => {
                native_window.set_fullscreen(native_window.window.fullscreen().is_none())
            }
            "ytu-library-folder" => open_folder(&state, &state.services.library_dir),
            "ytu-profile-folder" => open_folder(&state, &state.services.profile),
            "ytu-close" | "ytu-quit" => request_close(&state),
            #[cfg(target_os = "macos")]
            "ytu-clear-recent" => {
                if let Some(main) = objc2::MainThreadMarker::new() {
                    // SAFETY: this action runs on AppKit's main thread. The sender
                    // is optional and no document/file is deleted by this method.
                    unsafe {
                        objc2_app_kit::NSDocumentController::sharedDocumentController(main)
                            .clearRecentDocuments(None);
                    }
                    refresh_recent_menu();
                }
            }
            #[cfg(target_os = "macos")]
            id if id.starts_with("ytu-recent-") => {
                let path = RECENT_PATHS.with(|paths| {
                    paths
                        .borrow()
                        .iter()
                        .find(|(key, _)| key == id)
                        .map(|(_, path)| path.clone())
                });
                if let Some(path) = path {
                    let state = state.clone();
                    dioxus::core::spawn_forever(async move {
                        state.import_paths(vec![path]).await;
                    });
                }
            }
            _ => {}
        }
    });
    let import_state = close_state.clone();
    let import_window = window.clone();
    use_hook(move || {
        spawn(async move {
            // Drain fresh launch requests even while importing a large folder.
            // Only the on-disk mailbox expires messages; accepted requests stay
            // queued here and one importer processes them in arrival order.
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            let profile = import_state.services.profile.clone();
            let importer = import_open_requests(receiver, move |paths| {
                let state = import_state.clone();
                async move { state.import_paths(paths).await }
            });
            let collector = async move {
                loop {
                    let profile = profile.clone();
                    let requests = tokio::task::spawn_blocking(move || {
                        crate::native_instance::take_requests(&profile)
                    })
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or_default();
                    if !requests.is_empty() {
                        import_window.window.set_minimized(false);
                        import_window.window.set_visible(true);
                        import_window.window.set_focus();
                        let paths = requests.into_iter().flatten().collect::<Vec<_>>();
                        if !paths.is_empty() && sender.send(paths).is_err() {
                            return;
                        }
                    }
                    let paths = OPEN_FILES
                        .lock()
                        .map(|mut pending| std::mem::take(&mut *pending))
                        .unwrap_or_default();
                    if !paths.is_empty() && sender.send(paths).is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            };
            tokio::join!(collector, importer);
        })
    });
    use_wry_event_handler(move |event, _| match event {
        Event::WindowEvent {
            window_id,
            event: WindowEvent::CloseRequested,
            ..
        } if *window_id == window.id() => {
            if crate::updates::request_exit(&close_state) {
                window.set_close_behavior(dioxus::desktop::WindowCloseBehaviour::WindowHides);
            } else {
                prepare_close(&close_state);
            }
        }
        Event::Reopen {
            has_visible_windows: false,
            ..
        } if !window.window.is_visible() || window.window.is_minimized() => {
            // AppKit owns the focus of open/save panels. Activating an already
            // visible application must not focus the parent behind its sheet.
            window.window.set_minimized(false);
            window.window.set_visible(true);
            window.window.set_focus();
        }
        _ => {}
    });
}

fn prepare_close(state: &AppState) {
    record_log(&state.services.profile, "Application stopping");
    state.cancel_remote();
    if state.settings.peek().developer_mode {
        if let Ok(host) = &state.services.plugins {
            let _ = host.dispatch_event(yt_plugins::PluginEvent {
                name: "app.stopping".into(),
                payload: serde_json::json!({}),
            });
        }
    }
    for cancellation in state.cancellations.peek().values() {
        cancellation.cancel();
    }
    state.persist_playback();
}

pub(crate) fn open_folder(state: &AppState, path: &std::path::Path) {
    let executable = if cfg!(target_os = "macos") {
        "/usr/bin/open"
    } else {
        "explorer.exe"
    };
    let path = path.to_owned();
    let state = state.clone();
    dioxus::core::spawn_forever(async move {
        let result =
            tokio::task::spawn_blocking(move || Command::new(executable).arg(path).status()).await;
        match result {
            Ok(Ok(status)) if status.success() => {}
            Ok(Ok(status)) => state.notify(format!("No se pudo abrir la carpeta: {status}")),
            Ok(Err(error)) => state.notify(format!("No se pudo abrir la carpeta: {error}")),
            Err(error) => state.notify(format!("No se pudo abrir la carpeta: {error}")),
        }
    });
}

/// Copy through the operating system, on the UI thread, without browser permissions.
pub(crate) fn copy_text(state: &AppState, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Err(error) = write_clipboard_text(text) {
        state.notify(format!("No se pudo copiar el texto: {error}"));
    }
}

fn existing_file(path: &std::path::Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .ok()
        .filter(|path| path.is_file())
        .ok_or_else(|| "El archivo ya no está disponible.".to_owned())
}

fn openable_file(path: &std::path::Path) -> Result<PathBuf, String> {
    let path = existing_file(path)?;
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    if ![
        "mp3", "mp4", "m4a", "webm", "mkv", "wav", "flac", "aac", "ogg", "opus", "m3u", "m3u8",
    ]
    .iter()
    .any(|allowed| extension.eq_ignore_ascii_case(allowed))
    {
        return Err("Solo se pueden abrir archivos multimedia con otra aplicación.".into());
    }
    Ok(path)
}

fn browser_url(value: &str) -> Result<String, String> {
    let url = url::Url::parse(value).map_err(|_| "El enlace no es válido.".to_owned())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || value.chars().any(char::is_control)
    {
        return Err("Solo se pueden abrir enlaces HTTP o HTTPS sin credenciales.".into());
    }
    Ok(url.into())
}

fn run_native_action(
    state: &AppState,
    action: impl FnOnce() -> Result<(), String> + Send + 'static,
) {
    let state = state.clone();
    dioxus::core::spawn_forever(async move {
        match tokio::task::spawn_blocking(action).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => state.notify(error),
            Err(error) => state.notify(format!("No se pudo completar la acción: {error}")),
        }
    });
}

pub(crate) fn reveal_file(state: &AppState, path: &std::path::Path) {
    match existing_file(path) {
        Ok(path) => run_native_action(state, move || reveal_native(&path)),
        Err(error) => state.notify(error),
    }
}

pub(crate) fn open_default_file(state: &AppState, path: &std::path::Path) {
    match openable_file(path) {
        Ok(path) => run_native_action(state, move || open_native(path.as_os_str())),
        Err(error) => state.notify(error),
    }
}

pub(crate) fn open_url(state: &AppState, value: &str) {
    match browser_url(value) {
        Ok(url) => run_native_action(state, move || open_native(std::ffi::OsStr::new(&url))),
        Err(error) => state.notify(error),
    }
}

#[cfg(target_os = "macos")]
fn open_native(value: &std::ffi::OsStr) -> Result<(), String> {
    command_status(Command::new("/usr/bin/open").arg(value))
}

#[cfg(target_os = "macos")]
fn reveal_native(path: &std::path::Path) -> Result<(), String> {
    command_status(Command::new("/usr/bin/open").arg("-R").arg(path))
}

#[cfg(target_os = "macos")]
fn command_status(command: &mut Command) -> Result<(), String> {
    match command.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("El sistema no pudo abrir el elemento: {status}")),
        Err(error) => Err(format!("El sistema no pudo abrir el elemento: {error}")),
    }
}

#[cfg(target_os = "windows")]
mod shell {
    use std::{
        ffi::{c_void, OsStr},
        os::windows::ffi::OsStrExt,
        ptr,
    };

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            window: *mut c_void,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> isize;
        fn SHParseDisplayName(
            name: *const u16,
            context: *mut c_void,
            item: *mut *mut c_void,
            attributes: u32,
            output_attributes: *mut u32,
        ) -> i32;
        fn SHOpenFolderAndSelectItems(
            item: *const c_void,
            count: u32,
            children: *const *const c_void,
            flags: u32,
        ) -> i32;
    }
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoInitializeEx(reserved: *mut c_void, flags: u32) -> i32;
        fn CoUninitialize();
        fn CoTaskMemFree(memory: *mut c_void);
    }

    struct Com(bool);
    impl Com {
        fn initialize() -> Result<Self, String> {
            // SAFETY: no reserved data; initialize this worker for shell calls.
            let result = unsafe { CoInitializeEx(ptr::null_mut(), 0x2 | 0x4) };
            if result < 0 && result != 0x80010106u32 as i32 {
                return Err(format!("No se pudo iniciar el explorador: 0x{result:08x}"));
            }
            Ok(Self(result >= 0))
        }
    }
    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: balances this thread's successful CoInitializeEx.
                unsafe {
                    CoUninitialize();
                }
            }
        }
    }

    fn wide(value: &OsStr) -> Vec<u16> {
        let units = value.encode_wide().collect::<Vec<_>>();
        let prefix = "\\\\?\\".encode_utf16().collect::<Vec<_>>();
        let unc = "\\\\?\\UNC\\".encode_utf16().collect::<Vec<_>>();
        let mut result = if units.starts_with(&unc) {
            "\\\\"
                .encode_utf16()
                .chain(units[unc.len()..].iter().copied())
                .collect()
        } else if units.starts_with(&prefix) {
            units[prefix.len()..].to_vec()
        } else {
            units
        };
        result.push(0);
        result
    }

    pub(super) fn open(value: &OsStr) -> Result<(), String> {
        let _com = Com::initialize()?;
        let value = wide(value);
        let operation = wide(OsStr::new("open"));
        // SAFETY: strings remain alive and nul-terminated for this call. No
        // executable arguments are supplied; callers validate media paths/URLs.
        let result = unsafe {
            ShellExecuteW(
                ptr::null_mut(),
                operation.as_ptr(),
                value.as_ptr(),
                ptr::null(),
                ptr::null(),
                1,
            )
        };
        if result > 32 {
            Ok(())
        } else {
            Err(format!("El sistema no pudo abrir el elemento: {result}"))
        }
    }

    pub(super) fn reveal(path: &std::path::Path) -> Result<(), String> {
        let _com = Com::initialize()?;
        let path = wide(path.as_os_str());
        let mut item = ptr::null_mut();
        // SAFETY: writable PIDL output and a valid nul-terminated file path.
        let parsed = unsafe {
            SHParseDisplayName(
                path.as_ptr(),
                ptr::null_mut(),
                &mut item,
                0,
                ptr::null_mut(),
            )
        };
        if parsed < 0 {
            return Err(format!("No se pudo localizar el archivo: 0x{parsed:08x}"));
        }
        // With count zero, the absolute PIDL selects this file in its parent.
        // SAFETY: item came from SHParseDisplayName and is freed exactly once.
        let result = unsafe {
            let result = SHOpenFolderAndSelectItems(item, 0, ptr::null(), 0);
            CoTaskMemFree(item);
            result
        };
        if result >= 0 {
            Ok(())
        } else {
            Err(format!("No se pudo mostrar el archivo: 0x{result:08x}"))
        }
    }
}

#[cfg(target_os = "windows")]
fn open_native(value: &std::ffi::OsStr) -> Result<(), String> {
    shell::open(value)
}
#[cfg(target_os = "windows")]
fn reveal_native(path: &std::path::Path) -> Result<(), String> {
    shell::reveal(path)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn open_native(_: &std::ffi::OsStr) -> Result<(), String> {
    Err("Disponible en Windows y Mac.".into())
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn reveal_native(_: &std::path::Path) -> Result<(), String> {
    Err("Disponible en Windows y Mac.".into())
}

/// Shared close entry point for menus, caption buttons and OS media controls.
pub(crate) fn request_close(state: &AppState) {
    if !crate::updates::request_exit(state) {
        finish_close(state);
    }
}

/// Complete a previously deferred close without requesting another update check.
pub(crate) fn finish_close(state: &AppState) {
    prepare_close(state);
    let window = dioxus::desktop::window();
    window.set_close_behavior(dioxus::desktop::WindowCloseBehaviour::WindowCloses);
    window.close();
}

pub(crate) fn restore_after_update(_state: &AppState) {
    let window = dioxus::desktop::window();
    window.set_close_behavior(dioxus::desktop::WindowCloseBehaviour::WindowCloses);
    window.window.set_visible(true);
    window.window.set_minimized(false);
    window.window.set_focus();
}

#[cfg(test)]
mod native_tests {
    use super::*;
    use std::{future::Future, task::Context};

    #[tokio::test]
    async fn slow_import_keeps_later_launch_requests_after_mailbox_expiry() {
        use std::{cell::RefCell, rc::Rc, time::SystemTime};

        let temporary = tempfile::tempdir().unwrap();
        let profile = temporary.path().join("profile");
        let _primary = crate::native_instance::acquire(&profile).unwrap().unwrap();
        let first = temporary.path().join("large folder");
        std::fs::create_dir(&first).unwrap();
        let second = temporary.path().join("second song.wav");
        std::fs::write(&second, b"fixture").unwrap();
        let first = first.canonicalize().unwrap();
        let second = second.canonicalize().unwrap();
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let write_request = |name: &str, created, path: &PathBuf| {
            std::fs::write(
                profile.join(".instance").join(name),
                serde_json::to_vec(&serde_json::json!({"created":created,"paths":[path]})).unwrap(),
            )
            .unwrap();
        };
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (started, first_started) = tokio::sync::oneshot::channel();
        let (release, first_waiting) = tokio::sync::oneshot::channel();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let observed = calls.clone();
        let mut started = Some(started);
        let mut first_waiting = Some(first_waiting);
        let importer = import_open_requests(receiver, move |paths| {
            let calls = calls.clone();
            let started = started.take();
            let waiting = first_waiting.take();
            async move {
                calls.borrow_mut().push(paths);
                if let Some(started) = started {
                    started.send(()).unwrap();
                }
                if let Some(waiting) = waiting {
                    waiting.await.unwrap();
                }
            }
        });
        let collector = async {
            write_request("request-first.json", now, &first);
            for paths in crate::native_instance::take_requests(&profile).unwrap() {
                sender.send(paths).unwrap();
            }
            first_started.await.unwrap();
            write_request("request-second.json", now, &second);
            for paths in crate::native_instance::take_requests(&profile).unwrap() {
                sender.send(paths).unwrap();
            }

            // Model an import taking over 60 seconds without a real clock wait.
            // The same request would now expire if left on disk by the old
            // synchronous poller; the accepted copy must remain in the queue.
            write_request("request-expired-copy.json", now - 61, &second);
            assert!(crate::native_instance::take_requests(&profile)
                .unwrap()
                .is_empty());
            assert_eq!(*observed.borrow(), vec![vec![first.clone()]]);
            release.send(()).unwrap();
            drop(sender);
        };
        tokio::join!(collector, importer);
        assert_eq!(*observed.borrow(), vec![vec![first], vec![second]]);
    }

    #[test]
    fn native_drag_end_keeps_self_drop_blocked_until_browser_acknowledges() {
        let drags = Arc::new(OutgoingMediaDrags::default());
        assert!(!drags.active());
        let session = drags.begin();
        let (native_end, ended) = tokio::sync::oneshot::channel();
        let (browser_ack, delivered) = tokio::sync::oneshot::channel();
        let mut completion = Box::pin(finish_outgoing_drag(session, ended, async move {
            delivered.await.unwrap();
        }));
        let mut context = Context::from_waker(std::task::Waker::noop());

        assert!(completion.as_mut().poll(&mut context).is_pending());
        assert!(drags.active());
        native_end.send(()).unwrap();
        assert!(completion.as_mut().poll(&mut context).is_pending());
        // DoDragDrop has returned, but its queued self-drop is still rejected.
        assert!(drags.active());
        browser_ack.send(()).unwrap();
        assert!(completion.as_mut().poll(&mut context).is_ready());
        assert!(!drags.active());
    }

    #[test]
    fn failed_native_drag_start_unblocks_imports_without_a_callback() {
        let drags = Arc::new(OutgoingMediaDrags::default());
        let session = drags.begin();
        let (native_end, ended) = tokio::sync::oneshot::channel::<()>();
        let (browser_ack, delivered) = tokio::sync::oneshot::channel();
        let mut completion = Box::pin(finish_outgoing_drag(session.clone(), ended, async move {
            delivered.await.unwrap();
        }));
        let mut context = Context::from_waker(std::task::Waker::noop());

        // A start_drag error has no active OS session and may never call back.
        session.finish();
        assert!(!drags.active());
        drop(native_end);
        assert!(completion.as_mut().poll(&mut context).is_pending());
        assert!(!drags.active());
        browser_ack.send(()).unwrap();
        assert!(completion.as_mut().poll(&mut context).is_ready());
        assert!(!drags.active());
    }

    #[test]
    fn late_native_completion_cannot_release_a_new_drag() {
        let drags = Arc::new(OutgoingMediaDrags::default());
        let previous = drags.begin();
        let (native_end, ended) = tokio::sync::oneshot::channel();
        let mut completion = Box::pin(finish_outgoing_drag(previous.clone(), ended, async {}));
        let mut context = Context::from_waker(std::task::Waker::noop());
        let current = drags.begin();
        assert_ne!(previous.generation, current.generation);
        native_end.send(()).unwrap();
        assert!(completion.as_mut().poll(&mut context).is_ready());
        assert!(drags.active());
        previous.finish();
        assert!(drags.active());
        current.finish();
        assert!(!drags.active());
    }

    #[test]
    fn browser_actions_reject_credentials_and_non_web_protocols() {
        for value in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://user:password@example.com/",
            "https://example.com/\n",
        ] {
            assert!(browser_url(value).is_err(), "{value}");
        }
        assert_eq!(
            browser_url("https://example.com/a?q=hello%20world").unwrap(),
            "https://example.com/a?q=hello%20world"
        );
    }

    #[test]
    fn external_file_actions_reject_programs_and_follow_canonical_extension() {
        let temporary = tempfile::tempdir().unwrap();
        let media = temporary.path().join("A song.MP3");
        std::fs::write(&media, b"test").unwrap();
        assert!(openable_file(&media).is_ok());
        let executable = temporary.path().join("program.exe");
        std::fs::write(&executable, b"test").unwrap();
        assert!(openable_file(&executable).is_err());
        assert!(openable_file(temporary.path()).is_err());
        #[cfg(unix)]
        {
            let link = temporary.path().join("pretend.mp3");
            std::os::unix::fs::symlink(&executable, &link).unwrap();
            assert!(openable_file(&link).is_err());
        }
    }
}
