use crate::{icons::Icon, state::AppState};
use dioxus::prelude::*;
use yt_plugins::{PluginHost, PluginInfo};

/// Host operations may finish after their card is removed. Keep persistence
/// and the shared library refresh in the app scope, while the card owns only
/// its optional result receiver and UI signals.
fn run_plugin_task<T: Send + 'static>(
    state: AppState,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> tokio::sync::oneshot::Receiver<Result<T, String>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    dioxus::core::spawn_forever(async move {
        let result = tokio::task::spawn_blocking(work)
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| value);
        state.refresh();
        let mut revision = state.plugins_revision;
        revision += 1;
        if let Err(Err(error)) = sender.send(result) {
            state.notify_error(format!(
                "No se pudo completar la operación del plugin: {error}"
            ));
        }
    });
    receiver
}

#[component]
pub fn PluginPanel() -> Element {
    let state = use_context::<AppState>();
    let host = state.services.plugins.clone();
    let mut plugins = use_signal(Vec::<PluginInfo>::new);
    let mut error = use_signal(|| None::<String>);
    let mut loading = use_signal(|| false);
    let load_host = host.clone();
    let refresh = move || {
        if *loading.peek() {
            return;
        }
        loading.set(true);
        let host = load_host.clone();
        spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                host.and_then(|host| host.plugins().map_err(|error| error.to_string()))
            })
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result);
            loading.set(false);
            match result {
                Ok(list) => {
                    plugins.set(list);
                    error.set(None);
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };
    use_hook({
        let mut refresh = refresh.clone();
        move || refresh()
    });
    let mut reload = refresh.clone();
    let rows = plugins.read().clone();
    let mut scaffold_open = use_signal(|| false);
    let revision = state.plugins_revision;
    let mut refresh_revision = refresh.clone();
    use_effect(move || {
        let _ = revision.read();
        refresh_revision();
    });
    let mut section = state.settings_section;
    use_effect(move || {
        let requested = section.read().clone();
        if requested == "plugin-scaffold" {
            scaffold_open.set(true);
            section.set(String::new());
        }
    });
    let mut scaffold_id = use_signal(String::new);
    let mut scaffold_name = use_signal(String::new);
    let mut scaffold_description = use_signal(String::new);
    let mut creating = use_signal(|| false);
    let scaffold_host = host.clone();
    let scaffold_state = state.clone();
    let scaffold_refresh = refresh.clone();
    let open_host = host.clone();
    let open_state = state.clone();
    let plugin_directory = host
        .as_ref()
        .map(|host| host.directory().display().to_string())
        .unwrap_or_default();
    rsx! { section { class: "settings-section settings-section--developer",
        div { class: "settings-section__header",
            div { class: "settings-section__icon", PluginGlyph { name: "code", size: 15 } }
            div { div { class: "settings-section__title", "Plugins" }
                div { class: "settings-section__sub", "Código local de confianza · API v2" }
            }
            div { class: "settings-section__actions",
                button { r#type: "button", class: "btn-icon-sm", title: "Recargar", aria_label: "Recargar plugins", disabled: *loading.read(), onclick: move |_| reload(),
                    PluginGlyph { name: "refresh", size: 14, spinning: *loading.read() }
                }
                button { r#type: "button", class: "btn-icon-sm", title: "Crear plugin", onclick: move |_| scaffold_open.set(true), Icon { name: "plus", size: 14 } }
            }
        }
        div { class: "developer-warning", PluginGlyph { name: "shield", size: 15 }
            "Los plugins solo usan los permisos que autorices. Revisa su código antes de activarlos."
        }
        if let Ok(host) = &host {
            div { class: "plugin-examples",
                div { class: "plugin-examples__header", div {
                    div { class: "plugin-examples__title", "Ejemplos incluidos" }
                    div { class: "plugin-examples__description", "Aprende desde un comando sencillo hasta eventos y estado persistente." }
                } }
                div { class: "plugin-examples__grid",
                    for advanced in [false, true] {
                        PluginExampleCard {
                            key: "{advanced}", advanced, host: host.clone(),
                            installed: rows.iter().any(|plugin| plugin.id == if advanced { yt_plugins::JOURNAL_PLUGIN_ID } else { yt_plugins::EXAMPLE_PLUGIN_ID }),
                            changed: { let mut callback = refresh.clone(); move |_| callback() },
                        }
                    }
                }
            }
        }
        if let Some(error) = error.read().clone() { div { class: "settings-empty plugin-card__error", "{error}" } }
        if rows.is_empty() && !*loading.read() { div { class: "settings-empty", "No hay plugins instalados. Elige un ejemplo o crea uno desde la plantilla." } }
        if let Ok(host) = &host {
            for plugin in rows { PluginCard { key: "{plugin.id}", plugin, host: host.clone(), changed: { let mut callback = refresh.clone(); move |_| callback() } } }
        }
        button { r#type: "button", class: "settings-row", onclick: move |_| { if let Ok(host) = &open_host { crate::native::open_folder(&open_state, host.directory()); } },
            span { class: "settings-row__main", PluginGlyph { name: "folder", size: 15 }
                span { class: "settings-row__label",
                    span { class: "settings-row__title", "Abrir carpeta de plugins" }
                    span { class: "settings-row__sub", "{plugin_directory}" }
                }
            }
            PluginGlyph { name: "chevron", size: 13 }
        }
        if *scaffold_open.read() {
            div { class: "modal-backdrop", onclick: move |_| scaffold_open.set(false),
                form { class: "modal-panel plugin-scaffold", role: "dialog", aria_modal: "true", aria_label: "Crear plugin",
                    onclick: |event|event.stop_propagation(),
                    onsubmit: move |event| {
                        event.prevent_default();
                        if *creating.peek() { return; }
                        if let Ok(host) = &scaffold_host {
                            let host = host.clone();
                            let id = scaffold_id.peek().clone();
                            let name = scaffold_name.peek().clone();
                            let description = scaffold_description.peek().clone();
                            creating.set(true);
                            let state = scaffold_state.clone();
                            let receiver = run_plugin_task(state.clone(), move || host.scaffold(&id, &name, &description).map_err(|error|error.to_string()));
                            let mut refresh = scaffold_refresh.clone();
                            spawn(async move {
                                let result = receiver.await;
                                creating.set(false);
                                match result {
                                    Ok(Ok(path)) => { scaffold_open.set(false); refresh(); state.notify_success(format!("Plugin creado y desactivado: {}", path.display())); }
                                    Ok(Err(error)) => state.notify_error(error),
                                    Err(error) => state.notify_error(error.to_string()),
                                }
                            });
                        }
                    },
                    div { class: "modal-header", div { div { class: "modal-title", "Crear plugin" } div { class: "page-subtitle", "Genera una plantilla lista para editar" } }
                        button { r#type: "button", class: "btn-icon-sm", title: "Cerrar", onclick: move |_| scaffold_open.set(false), Icon { name: "close", size: 14 } }
                    }
                    label { span { "ID" } input { aria_label: "ID del plugin", placeholder: "mi.plugin", value: "{scaffold_id}", oninput: move |event|scaffold_id.set(event.value()) } }
                    label { span { "Nombre" } input { aria_label: "Nombre del plugin", placeholder: "Mi plugin", value: "{scaffold_name}", oninput: move |event|scaffold_name.set(event.value()) } }
                    label { span { "Descripción" } textarea { aria_label: "Descripción del plugin", placeholder: "Qué hace este plugin", value: "{scaffold_description}", oninput: move |event|scaffold_description.set(event.value()) } }
                    div { class: "modal-actions",
                        button { r#type: "button", class: "btn-secondary", onclick: move |_| scaffold_open.set(false), "Cancelar" }
                        button { r#type: "submit", class: "btn-accent", disabled: *creating.read() || scaffold_id.read().trim().is_empty() || scaffold_name.read().trim().is_empty(),
                            if *creating.read() { PluginGlyph { name: "refresh", size: 13, spinning: true } } else { Icon { name: "plus", size: 13 } }
                            if *creating.read() { "Creando…" } else { "Crear" }
                        }
                    }
                }
            }
        }
    } }
}

#[derive(Props, Clone)]
struct PluginExampleCardProps {
    advanced: bool,
    installed: bool,
    host: PluginHost,
    changed: EventHandler<()>,
}
impl PartialEq for PluginExampleCardProps {
    fn eq(&self, _: &Self) -> bool {
        false
    }
}

#[allow(non_snake_case)]
fn PluginExampleCard(props: PluginExampleCardProps) -> Element {
    let state = use_context::<AppState>();
    let mut installing = use_signal(|| false);
    let manifest = if props.advanced {
        include_str!("../../yt-plugins/examples/rust-journal/plugin.json")
    } else {
        include_str!("../../yt-plugins/examples/rust-greeter/plugin.json")
    };
    let example: yt_plugins::PluginManifest =
        serde_json::from_str(manifest).expect("bundled example manifest");
    let level = if props.advanced { "advanced" } else { "basic" };
    let level_label = if props.advanced {
        "Avanzado"
    } else {
        "Básico"
    };
    let permissions = example.permissions.len();
    rsx! { div { class: "plugin-example-card",
        div { class: "plugin-example-card__body",
            div { class: "plugin-example-card__eyebrow",
                span { class: "plugin-example-level plugin-example-level--{level}", "{level_label}" }
                span { "Ejemplo oficial" }
            }
            div { class: "plugin-example-card__title", "{example.name}" }
            div { class: "plugin-example-card__description", "{example.description}" }
            div { class: "plugin-example-card__meta",
                if permissions == 0 { "Sin permisos especiales" } else if permissions == 1 { "1 permiso declarado" } else { "{permissions} permisos declarados" }
                " · v{example.version}"
            }
        }
        button { r#type: "button", class: "btn-secondary plugin-example-card__action", disabled: props.installed || *installing.read(), onclick: move |_| {
            installing.set(true);
            let host = props.host.clone(); let state = state.clone();
            let receiver = run_plugin_task(state.clone(), move || {
                (if props.advanced { host.install_journal_example() } else { host.install_example() }).map_err(|error| error.to_string())
            });
            spawn(async move {
                let result = receiver.await;
                installing.set(false);
                match result {
                    Ok(Ok(created)) => { props.changed.call(()); state.notify_success(if created { "Ejemplo instalado y desactivado" } else { "El ejemplo ya está instalado" }); }
                    Ok(Err(error)) => state.notify_error(error),
                    Err(error) => state.notify_error(error.to_string()),
                }
            });
        },
            if *installing.read() { PluginGlyph { name: "refresh", size: 12, spinning: true } }
            if props.installed { "Instalado" } else if *installing.read() { "Instalando…" } else { "Instalar" }
        }
    } }
}

// The original Lucide paths, retaining the existing settings layout and CSS.
#[component]
fn PluginGlyph(name: &'static str, size: u32, #[props(default = false)] spinning: bool) -> Element {
    let paths: &[&str] = match name {
        "blocks" => &["M10 22V7a1 1 0 0 0-1-1H4a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-5a1 1 0 0 0-1-1H2", "M15 2h6a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1h-6a1 1 0 0 1-1-1V3a1 1 0 0 1 1-1Z"],
        "code" => &["m18 16 4-4-4-4", "m6 8-4 4 4 4", "m14.5 4-5 16"],
        "refresh" => &["M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8", "M21 3v5h-5", "M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16", "M8 16H3v5"],
        "shield" => &["M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z", "M12 8v4", "M12 16h.01"],
        "folder" => &["m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.94 1.5H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h3.93a2 2 0 0 1 1.66.9l.82 1.2a2 2 0 0 0 1.66.9H18a2 2 0 0 1 2 2v2"],
        _ => &["m9 18 6-6-6-6"],
    };
    rsx! { svg { class: if spinning { "spinner" } else { "" }, width: size, height: size, view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round", "aria-hidden": "true",
        for path in paths { path { d: *path } }
    } }
}

#[derive(Props, Clone)]
struct PluginCardProps {
    plugin: PluginInfo,
    host: PluginHost,
    changed: EventHandler<()>,
}
impl PartialEq for PluginCardProps {
    fn eq(&self, _: &Self) -> bool {
        false
    }
}

#[allow(non_snake_case)]
fn PluginCard(props: PluginCardProps) -> Element {
    let s = use_context::<AppState>();
    let plugin = props.plugin;
    let mut busy = use_signal(|| false);
    let mut output = use_signal(String::new);
    let mut config =
        use_signal(|| serde_json::to_string_pretty(&plugin.config).unwrap_or_else(|_| "{}".into()));
    let mut arguments = use_signal(|| "{}".to_owned());
    let host = props.host;
    let toggle_host = host.clone();
    let toggle_id = plugin.id.clone();
    let toggle_state = s.clone();
    let config_host = host.clone();
    let config_id = plugin.id.clone();
    let loaded = plugin.enabled && plugin.compatible && plugin.error.is_none();
    let status = if loaded {
        "Activo"
    } else if plugin.enabled {
        "Con error"
    } else {
        "Inactivo"
    };
    let status_class = if loaded {
        "plugin-status plugin-status--ok"
    } else if plugin.enabled {
        "plugin-status plugin-status--warn"
    } else {
        "plugin-status"
    };
    rsx! { div { class: "plugin-card",
        div { class: "plugin-card__icon", PluginGlyph { name: "blocks", size: 17 } }
        div { class: "plugin-card__body",
            div { class: "plugin-card__title", "{plugin.name}" span { class: status_class, "{status}" } }
            div { class: "plugin-card__meta", "{plugin.id} · v{plugin.version}" }
        if !plugin.description.is_empty() { div { class: "plugin-card__description", "{plugin.description}" } }
        if !plugin.compatible { p { class: "rust-settings-note", "Este plugin necesita una versión compatible." } }
        if let Some(error) = &plugin.error { p { class: "plugin-card__error", "{error}" } }
        if plugin.compatible {
            details { style: "margin-top:8px", summary { class: "plugin-card__description", "Permisos, comandos y configuración" }
            if !plugin.permissions.is_empty() {
                div { class: "rust-settings-note", "Permisos solicitados" }
                for permission in &plugin.permissions {
                    label { class: "settings-row", style: "gap:12px",
                        span { match permission.as_str() { "library.read" => "Consultar la biblioteca", "library.write" => "Modificar favoritos y playlists", "state" => "Guardar datos propios del plugin", "events" => "Recibir eventos de la aplicación", _ => "Permiso desconocido" } }
                        input { r#type: "checkbox", aria_label: "Permiso {permission} para {plugin.name}", checked: plugin.granted_permissions.contains(permission), disabled: *busy.read(),
                            onchange: { let host = host.clone(); let id = plugin.id.clone(); let permission = permission.clone(); let grants = plugin.granted_permissions.clone(); let state = s.clone();
                                move |event| { let mut permissions = grants.clone(); permissions.retain(|value| value != &permission); if event.checked() { permissions.push(permission.clone()); }
                                    let host = host.clone(); let id = id.clone(); let state = state.clone(); busy.set(true);
                                    let receiver = run_plugin_task(state.clone(), move || host.set_permissions(&id, permissions).map_err(|error|error.to_string()));
                                    spawn(async move { let result = receiver.await; busy.set(false);
                                        match result { Ok(Ok(())) => props.changed.call(()), Ok(Err(error)) => state.notify_error(error.to_string()), Err(error) => state.notify_error(error.to_string()) }
                                    });
                                }
                            }
                        }
                    }
                }
            }
            if !plugin.commands.is_empty() {
                details { style: "margin-top:12px", summary { "Argumentos de comandos" }
                    textarea { class: "rust-inline-input", style: "width:100%;min-height:70px;margin:10px 0;font-family:monospace", aria_label: "Argumentos de comandos de {plugin.name}", value: "{arguments}", oninput: move |e| arguments.set(e.value()) }
                }
            }
            for command in &plugin.commands {
                button { class: "btn-secondary", style: "margin:10px 8px 4px 0", disabled: !plugin.enabled || *busy.read(), onclick: {
                    let host = host.clone(); let id = plugin.id.clone(); let command = command.id.clone(); let state = s.clone();
                    move |_| { let args = match serde_json::from_str(&arguments.read()) { Ok(args) => args, Err(e) => { output.set(format!("Argumentos inválidos: {e}")); return; } };
                        let host = host.clone(); let id = id.clone(); let command = command.clone(); let state = state.clone(); busy.set(true);
                        let receiver = run_plugin_task(state, move || host.execute(&id, &command, args).map_err(|error|error.to_string()));
                        spawn(async move {
                        let result = receiver.await;
                        busy.set(false);
                        output.set(match result { Ok(Ok(result)) => serde_json::to_string_pretty(&result).unwrap_or_default(), Ok(Err(e)) => e.to_string(), Err(e) => e.to_string() });
                    }); }
                }, "{command.label}" }
            }
            details { style: "margin-top:12px", summary { "Configuración" }
                textarea { class: "rust-inline-input", style: "width:100%;min-height:90px;margin:10px 0;font-family:monospace", aria_label: "Configuración de {plugin.name}", value: "{config}", oninput: move |e| config.set(e.value()) }
                button { class: "btn-secondary", onclick: move |_| {
                    let text = config.read().clone();
                    match serde_json::from_str(&text) { Ok(value) => match config_host.set_config(&config_id, value) { Ok(_) => { props.changed.call(()); let mut revision = s.plugins_revision; revision += 1; s.notify_success("Configuración guardada"); }, Err(e) => s.notify_error(e.to_string()) }, Err(e) => s.notify_warning(format!("Configuración inválida: {e}")) }
                }, "Guardar configuración" }
            }
            if !output.read().is_empty() { pre { class: "rust-settings-note", style: "white-space:pre-wrap;overflow-wrap:anywhere", "{output}" } }
            }
        }
        }
            button { class: if plugin.enabled { "settings-switch active" } else { "settings-switch" }, role: "switch", aria_checked: plugin.enabled, aria_label: if plugin.enabled { format!("Desactivar {}", plugin.name) } else { format!("Activar {}", plugin.name) }, disabled: !plugin.compatible || *busy.read(), onclick: move |_| {
                let host = toggle_host.clone(); let id = toggle_id.clone(); let state = toggle_state.clone();
                busy.set(true);
                let receiver = run_plugin_task(state.clone(), move || host.set_enabled(&id, !plugin.enabled).map_err(|error|error.to_string()));
                spawn(async move {
                    let result = receiver.await;
                    busy.set(false);
                    match result { Ok(Ok(_)) => props.changed.call(()), Ok(Err(e)) => state.notify_error(e.to_string()), Err(e) => state.notify_error(e.to_string()) }
                });
            }, span {} }
    } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{use_app_state, Services};
    use std::{cell::RefCell, rc::Rc, time::Duration};

    type Capture = Rc<RefCell<Option<AppState>>>;
    fn harness() -> Element {
        let services = use_context::<Rc<Services>>();
        let capture = use_context::<Capture>();
        let state = use_app_state(services);
        *capture.borrow_mut() = Some(state);
        rsx! { div {} }
    }

    #[tokio::test]
    async fn plugin_mutation_refreshes_library_after_its_card_receiver_is_dropped() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let capture: Capture = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(harness);
        dom.insert_any_root_context(Box::new(services.clone()));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let state = capture.borrow().as_ref().unwrap().clone();
        let core = services.core.clone();
        dom.in_scope(ScopeId::ROOT, || {
            let receiver = run_plugin_task(state.clone(), move || {
                core.create_playlist("Created after leaving plugin settings")
                    .map_err(|error| error.to_string())
            });
            // Unmounting PluginCard drops this receiver while its host operation
            // is running. The shared state must still receive the DB mutation.
            drop(receiver);
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                dom.render_immediate_to_vec();
                if !state.playlists.peek().is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            state.playlists.peek()[0].name,
            "Created after leaving plugin settings"
        );
        assert_eq!(services.core.playlists().unwrap().len(), 1);
        assert!(state.last_notice().is_none());
    }
}
