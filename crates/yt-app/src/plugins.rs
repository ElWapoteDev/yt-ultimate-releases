use crate::{icons::Icon, state::AppState};
use dioxus::prelude::*;
use yt_plugins::{PluginHost, PluginInfo};

#[component]
pub fn PluginPanel() -> Element {
    let s = use_context::<AppState>();
    let host = use_hook(|| {
        PluginHost::open(s.services.profile.join("Plugins")).map_err(|e| e.to_string())
    });
    let mut plugins = use_signal(Vec::<PluginInfo>::new);
    let mut error = use_signal(|| None::<String>);
    let load_host = host.clone();
    let mut refresh = move || match &load_host {
        Ok(host) => match host.plugins() {
            Ok(list) => {
                plugins.set(list);
                error.set(None);
            }
            Err(e) => error.set(Some(e.to_string())),
        },
        Err(e) => error.set(Some(e.clone())),
    };
    use_hook(refresh.clone());
    let install_host = host.clone();
    let install_state = s.clone();
    let mut install_refresh = refresh.clone();
    let rows = plugins.read().clone();
    rsx! { section { class: "settings-section settings-section--developer",
        div { class: "settings-section__header", div { class: "settings-section__icon", Icon { name: "grid", size: 19 } }
            div { div { class: "settings-section__title", "Plugins" } div { class: "settings-section__sub", "Extensiones instaladas" } }
            button { class: "btn-secondary", onclick: move |_| refresh(), "Recargar" }
        }
        p { class: "rust-settings-note", "Cada plugin nuevo empieza desactivado. Los plugins de la versión anterior se conservan y requieren una versión compatible." }
        button { class: "settings-row", onclick: move |_| {
            if let Ok(host) = &install_host { match host.install_example() { Ok(created) => { install_refresh(); install_state.notify(if created { "Ejemplo instalado y desactivado" } else { "El ejemplo ya está instalado" }); }, Err(e) => install_state.notify(e.to_string()) } }
        }, Icon { name: "plus" } "Instalar ejemplo" }
        if let Some(error) = error.read().clone() { p { class: "rust-settings-note rust-danger", "{error}" } }
        if rows.is_empty() { p { class: "rust-settings-note", "No hay plugins instalados." } }
        if let Ok(host) = host {
            for plugin in rows { PluginCard { key: "{plugin.id}", plugin, host: host.clone(), changed: { let mut callback = refresh.clone(); move |_| callback() } } }
        }
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
    rsx! { div { class: "plugin-card", style: "display:block",
        div { style: "display:flex;align-items:center;justify-content:space-between;gap:12px",
            div { class: "plugin-card__body", div { class: "plugin-card__title", "{plugin.name}" } div { class: "plugin-card__meta", "{plugin.id} · v{plugin.version}" } }
            button { class: if plugin.enabled { "settings-switch active" } else { "settings-switch" }, role: "switch", aria_checked: plugin.enabled, aria_label: "Activar {plugin.name}", disabled: !plugin.compatible || *busy.read(), onclick: move |_| {
                let host = toggle_host.clone(); let id = toggle_id.clone(); let state = toggle_state.clone();
                busy.set(true);
                spawn(async move {
                    let result = tokio::task::spawn_blocking(move || host.set_enabled(&id, !plugin.enabled)).await;
                    busy.set(false);
                    match result { Ok(Ok(_)) => props.changed.call(()), Ok(Err(e)) => state.notify(e.to_string()), Err(e) => state.notify(e.to_string()) }
                });
            }, span {} }
        }
        p { class: "plugin-card__description", "{plugin.description}" }
        if !plugin.compatible { p { class: "rust-settings-note", "Este plugin necesita una versión compatible." } }
        if let Some(error) = &plugin.error { p { class: "plugin-card__error", "{error}" } }
        if plugin.compatible {
            if !plugin.commands.is_empty() {
                details { style: "margin-top:12px", summary { "Argumentos de comandos" }
                    textarea { class: "rust-inline-input", style: "width:100%;min-height:70px;margin:10px 0;font-family:monospace", aria_label: "Argumentos de comandos de {plugin.name}", value: "{arguments}", oninput: move |e| arguments.set(e.value()) }
                }
            }
            for command in &plugin.commands {
                button { class: "btn-secondary", style: "margin:10px 8px 4px 0", disabled: !plugin.enabled || *busy.read(), onclick: {
                    let host = host.clone(); let id = plugin.id.clone(); let command = command.id.clone();
                    move |_| { let args = match serde_json::from_str(&arguments.read()) { Ok(args) => args, Err(e) => { output.set(format!("Argumentos inválidos: {e}")); return; } };
                        let host = host.clone(); let id = id.clone(); let command = command.clone(); busy.set(true); spawn(async move {
                        let result = tokio::task::spawn_blocking(move || host.execute(&id, &command, args)).await;
                        busy.set(false);
                        output.set(match result { Ok(Ok(result)) => serde_json::to_string_pretty(&result).unwrap_or_default(), Ok(Err(e)) => e.to_string(), Err(e) => e.to_string() });
                    }); }
                }, "{command.label}" }
            }
            details { style: "margin-top:12px", summary { "Configuración" }
                textarea { class: "rust-inline-input", style: "width:100%;min-height:90px;margin:10px 0;font-family:monospace", aria_label: "Configuración de {plugin.name}", value: "{config}", oninput: move |e| config.set(e.value()) }
                button { class: "btn-secondary", onclick: move |_| {
                    let text = config.read().clone();
                    match serde_json::from_str(&text) { Ok(value) => match config_host.set_config(&config_id, value) { Ok(_) => { props.changed.call(()); s.notify("Configuración guardada"); }, Err(e) => s.notify(e.to_string()) }, Err(e) => s.notify(format!("Configuración inválida: {e}")) }
                }, "Guardar configuración" }
            }
            if !output.read().is_empty() { pre { class: "rust-settings-note", style: "white-space:pre-wrap;overflow-wrap:anywhere", "{output}" } }
        }
    } }
}
