use crate::{
    app::{EmptyState, Thumb},
    icons::Icon,
    state::{AppState, DownloadView},
};
use dioxus::prelude::*;

fn active(item: &DownloadView) -> bool {
    matches!(item.status.as_str(), "Preparando" | "Descargando")
}

fn phase(item: &DownloadView) -> (&str, &str, bool) {
    if item.status == "Preparando" {
        return ("En cola", "loader", true);
    }
    if item.status == "Completado" {
        return ("Completado", "check-circle", false);
    }
    if item.status == "Error" {
        return ("Error", "alert-circle", false);
    }
    if active(item) {
        if item.detail.starts_with("Uniendo") {
            return ("Combinando…", "loader", true);
        }
        if item.detail.starts_with("Convirtiendo") {
            return ("Convirtiendo…", "loader", true);
        }
        if item.detail.starts_with("Obteniendo") || item.detail.starts_with("Guardando") {
            return ("Procesando…", "loader", true);
        }
        if item.detail.contains("esperando") || item.detail.contains("Esperando") {
            return ("En cola", "loader", true);
        }
        return ("Descargando", "download", false);
    }
    (&item.status, "pause", false)
}

#[component]
pub fn DownloadsPage() -> Element {
    let state = use_context::<AppState>();
    let items = state.downloads.read().clone();
    let running = items.iter().filter(|item| active(item)).count();
    let completed = items
        .iter()
        .filter(|item| item.status == "Completado")
        .count();
    let paused = items.len() - running - completed;
    let clear_state = state.clone();
    rsx! { div { class: "page-shell",
        div { class: "toolbar", div { class: "toolbar__left", h1 { class: "page-title", "Descargas" }
            if running > 0 { span { class: "page-count", "{running} en curso" } }
        }
            if completed + paused > 0 { div { class: "toolbar__right", button { class: "btn-secondary", onclick: move |_| {
                let rows = clear_state.downloads.peek().clone();
                let initial=rows.iter().filter(|row|!active(row)).count();
                for row in rows.iter().filter(|row| !active(row)) { clear_state.remove_download_history(&row.id); }
                let remaining=clear_state.downloads.peek().iter().filter(|row|!active(row)).count();
                if remaining==0 {clear_state.notify("Historial limpio");} else {clear_state.notify(format!("{} registros quitados; {remaining} no se pudieron quitar",initial.saturating_sub(remaining)));}
            }, Icon { name: "trash", size: 13 } "Limpiar historial" } } }
        }
        if items.is_empty() { EmptyState { title: "No hay descargas", hint: "Desde Buscar puedes guardar cualquier canción o vídeo; el progreso aparecerá aquí.", icon: "download-cloud" } }
        for (label, count, group) in [("En curso", running, 0), ("Completadas", completed, 1), ("Pausadas y con errores", paused, 2)] {
            if count > 0 { div { class: "section",
                div { class: "section-header", h2 { class: "section-title", "{label}" } span { class: "section-count", "{count}" } }
                for item in items.iter().filter(|item| match group { 0 => active(item), 1 => item.status == "Completado", _ => !active(item) && item.status != "Completado" }).cloned() {
                    DownloadRow { key: "{item.id}", item }
                }
            } }
        }
    } }
}

#[component]
fn DownloadRow(item: DownloadView) -> Element {
    let state = use_context::<AppState>();
    let running = active(&item);
    let completed = item.status == "Completado";
    let color = if item.status == "Error" {
        "var(--system-red)"
    } else if completed {
        "var(--system-green)"
    } else if running {
        "var(--accent)"
    } else {
        "var(--text-tertiary)"
    };
    let pause = state.clone();
    let pause_id = item.id.clone();
    let discard = state.clone();
    let discard_id = item.id.clone();
    let retry = state.clone();
    let retry_id = item.id.clone();
    let remove = state.clone();
    let remove_id = item.id.clone();
    let percent = item.progress.clamp(0.0, 100.0);
    let (label, icon, spinning) = phase(&item);
    let label = label.to_owned();
    let playlist = use_hook(|| {
        state.services.core.job(&item.id).ok().and_then(|job| {
            let id = job.payload["playlist_id"].as_str()?.to_owned();
            let order =
                serde_json::from_value::<Vec<String>>(job.payload["playlist_order"].clone())
                    .ok()?;
            Some((id, order))
        })
    });
    let playlist_progress = playlist.as_ref().map(|(id, order)| {
        let done = state
            .library
            .read()
            .iter()
            .filter(|track| {
                !track.missing
                    && (track.media_type == "video") == item.video
                    && order.contains(&track.source_url)
            })
            .map(|track| &track.source_url)
            .collect::<std::collections::HashSet<_>>()
            .len();
        let name = state
            .playlists
            .read()
            .iter()
            .find(|p| &p.id == id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Playlist".into());
        (name, done, order.len())
    });
    rsx! { div { class: "task-row",
        div { style:"position:relative;flex-shrink:0",Thumb { src: item.thumbnail.clone(), size: "md", video: item.video }
            if let Some((_,done,total))=&playlist_progress {div{class:"playlist-badge","{done}/{total}"}}
        }
        div { class: "task-row__body", div { class: "task-row__title", "{item.title}" }
            div { class: "task-row__status", span { class:if spinning{"spinner"}else{""},style:"color:{color};display:inline-flex",Icon{name:icon,size:12} }span { style: "color:{color}","{label}" } }
            if let Some((name,done,total))=&playlist_progress {div{class:"task-row__status","{name} · {done} de {total}"}}
            if !item.detail.is_empty() { div { class: if item.status == "Error" { "task-row__error" } else { "task-row__status" }, "{item.detail}" } }
            if running && percent > 0.0 { div { class: "progress-track", role: "progressbar", aria_label: "Progreso de {item.title}", aria_valuenow: "{percent}", aria_valuemin: "0", aria_valuemax: "100", div { class: "progress-fill", style: "width:{percent}%" } } }
        }
        if running { span { class: "task-row__pct", "{percent.round()}%" }
            button { class: "btn-icon-sm", title: "Pausar descarga", onclick: move |_| pause.pause_download(&pause_id), Icon { name: "pause", size: 13 } }
        }
        if !running && !completed { button { class: "btn-secondary", onclick: move |_| retry.retry_download(&retry_id),
            if item.discarded { "Descargar de nuevo" } else if item.resumable && item.status != "Error" { "Reanudar" } else { "Reintentar" }
        } }
        if !completed && !item.discarded { button { class: "btn-icon-sm", title: "Descartar descarga parcial", onclick: move |_| discard.discard_download(&discard_id), Icon { name: "close", size: 13 } } }
        if !running { button { class: "btn-icon-sm", title: "Quitar del historial", onclick: move |_| remove.remove_download_history(&remove_id), Icon { name: "trash", size: 13 } } }
    } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{download_view, use_app_state, Services};
    use std::rc::Rc;
    use yt_core::JobStatus;
    fn harness() -> Element {
        let services = use_context::<Rc<Services>>();
        let state = use_app_state(services);
        use_context_provider(|| state);
        rsx! {DownloadsPage{}}
    }
    #[tokio::test]
    async fn downloads_render_real_phases_and_distinguish_paused_discarded_and_failed_actions() {
        let temp = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(temp.path()));
        for (title, status, message, result) in [
            (
                "Mux",
                JobStatus::Running,
                "Uniendo vídeo y audio",
                serde_json::json!({}),
            ),
            (
                "Paused",
                JobStatus::Canceled,
                "Pausada",
                serde_json::json!({"resumable":true}),
            ),
            (
                "Discarded",
                JobStatus::Canceled,
                "Descartada",
                serde_json::json!({"discarded":true}),
            ),
            (
                "Failed",
                JobStatus::Error,
                "Fallo de red",
                serde_json::json!({"resumable":true}),
            ),
        ] {
            let job=services.core.create_job("download",&serde_json::json!({"title":title,"video":true,"url":"https://www.youtube.com/watch?v=abcdefghijk"})).unwrap();
            let job = services
                .core
                .update_job(&job.id, status, 50.0, message, &result, "")
                .unwrap();
            if title == "Mux" {
                assert_eq!(phase(&download_view(&job)).0, "Combinando…");
            }
        }
        let mut dom = VirtualDom::new(harness);
        dom.insert_any_root_context(Box::new(services));
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        for text in [
            "Descargas",
            "Limpiar historial",
            "Reanudar",
            "Descargar de nuevo",
            "Reintentar",
            "Descartar descarga parcial",
            "Quitar del historial",
        ] {
            assert!(html.contains(text), "missing {text}");
        }
    }
}
