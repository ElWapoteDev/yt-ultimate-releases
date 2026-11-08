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
use std::{path::PathBuf, process::Command, sync::Mutex, time::Duration};

// macOS can deliver document events before the first Dioxus component mounts.
// Keep those paths until the application's importer is available.
static OPEN_FILES: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

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
            &item("play-pause", "Reproducir / Pausar", Code::Space, false),
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

pub fn use_native(mut state: AppState) {
    let window = use_window();
    let native_window = window.clone();
    let close_state = state.clone();
    use_muda_event_handler(move |event| match event.id.0.as_str() {
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
        "ytu-close" | "ytu-quit" => {
            prepare_close(&state);
            native_window.close();
        }
        _ => {}
    });
    let import_state = close_state.clone();
    use_hook(move || {
        spawn(async move {
            loop {
                let paths = OPEN_FILES
                    .lock()
                    .map(|mut pending| std::mem::take(&mut *pending))
                    .unwrap_or_default();
                if !paths.is_empty() {
                    import_state.import_paths(paths).await;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
    });
    use_wry_event_handler(move |event, _| match event {
        Event::WindowEvent {
            window_id,
            event: WindowEvent::CloseRequested,
            ..
        } if *window_id == window.id() => prepare_close(&close_state),
        Event::Reopen { .. } => {
            window.window.set_visible(true);
            window.window.set_focus();
        }
        _ => {}
    });
}

fn prepare_close(state: &AppState) {
    for cancellation in state.cancellations.peek().values() {
        cancellation.cancel();
    }
    state.persist_playback();
}

fn open_folder(state: &AppState, path: &std::path::Path) {
    let executable = if cfg!(target_os = "macos") {
        "/usr/bin/open"
    } else {
        "explorer.exe"
    };
    let path = path.to_owned();
    let state = state.clone();
    spawn(async move {
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
