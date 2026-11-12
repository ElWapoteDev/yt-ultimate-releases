use crate::{
    activity::{overall_progress, Notice, Stop, Task, Tone},
    app::EmptyState,
    icons::Icon,
    state::{AppState, Page},
};
use dioxus::prelude::*;

/// Radius, in SVG units of an 18-unit box, of the activity button's progress ring.
const RING_RADIUS: f64 = 7.0;

fn open_page(state: &AppState, page: Page) {
    let (mut current, mut open) = (state.page, state.activity.open);
    current.set(page);
    open.set(false);
}

/// Toast stack for recent notices. Mounted once at the root; also carries the
/// activity stylesheet for the button and the panel.
#[component]
pub fn Toasts() -> Element {
    let state = use_context::<AppState>();
    let activity = state.activity;
    let toasts = activity.toasts.read().clone();
    let notices = activity.notices.read();
    let visible = toasts
        .iter()
        .filter_map(|toast| notices.iter().find(|notice| notice.id == toast.id).cloned())
        .collect::<Vec<_>>();
    rsx! {
        style { {include_str!("activity_ui.css")} }
        if !visible.is_empty() && !*activity.open.read() {
            div {
                class: "toast-stack",
                role: "region",
                aria_label: "Avisos",
                onmouseenter: move |_| activity.hover(true),
                onmouseleave: move |_| activity.hover(false),
                for notice in visible {
                    ToastCard { key: "{notice.id}", notice }
                }
            }
        }
    }
}

#[component]
fn ToastCard(notice: Notice) -> Element {
    let state = use_context::<AppState>();
    let id = notice.id;
    let dismiss = state.clone();
    let follow = state.clone();
    rsx! {
        div {
            class: "toast toast--{notice.tone.class()}",
            role: if notice.tone == Tone::Error { "alert" } else { "status" },
            Icon {
                name: notice.tone.icon(),
                size: 15,
                class: "toast__icon",
            }
            span {
                class: "toast__text",
                "{notice.text}"
                if notice.repeats > 0 {
                    span { class: "toast__count", " ×{notice.repeats + 1}" }
                }
            }
            if let Some(page) = notice.page {
                button {
                    class: "toast__action",
                    onclick: move |_| {
                        open_page(&follow, page);
                        follow.activity.dismiss(id);
                    },
                    "Ver"
                }
            }
            button {
                class: "btn-icon-sm toast__close",
                aria_label: "Cerrar aviso",
                onclick: move |_| dismiss.activity.dismiss(id),
                Icon { name: "close", size: 13 }
            }
        }
    }
}

fn summary(tasks: &[Task], unread: usize) -> String {
    match tasks {
        [] if unread == 1 => "1 aviso nuevo".into(),
        [] if unread > 1 => format!("{unread} avisos nuevos"),
        [] => "Actividad".into(),
        [task] => match task.progress {
            Some(progress) => format!("{} · {progress:.0}%", task.title),
            None => task.title.clone(),
        },
        _ => format!("{} tareas en curso", tasks.len()),
    }
}

/// Player-bar entry to the activity panel: a progress ring while work runs and
/// a dot for notices the user has not seen.
#[component]
pub fn ActivityButton() -> Element {
    let state = use_context::<AppState>();
    let tasks = state.running_tasks();
    let unread = *state.activity.unread.read();
    let open = *state.activity.open.read();
    let title = summary(&tasks, unread.count);
    let progress = overall_progress(&tasks);
    let busy = !tasks.is_empty();
    rsx! {
        button {
            class: if open { "player-btn player-btn-small activity-button player-btn-active" } else { "player-btn player-btn-small activity-button" },
            title: "{title}",
            aria_label: "Actividad: {title}",
            aria_expanded: "{open}",
            onclick: move |_| state.toggle_activity(),
            if busy {
                ProgressRing { progress }
            } else {
                Icon { name: "bell", size: 15 }
            }
            if unread.count > 0 && !open {
                span { class: if unread.errors > 0 { "activity-dot activity-dot--error" } else { "activity-dot" } }
            }
        }
    }
}

#[component]
fn ProgressRing(progress: Option<f64>) -> Element {
    let circumference = 2.0 * std::f64::consts::PI * RING_RADIUS;
    let offset = progress.map(|value| circumference * (1.0 - value / 100.0));
    rsx! {
        svg {
            class: if offset.is_none() { "activity-ring activity-ring--spinning" } else { "activity-ring" },
            width: "18",
            height: "18",
            view_box: "0 0 18 18",
            "aria-hidden": "true",
            circle {
                class: "activity-ring__track",
                cx: "9",
                cy: "9",
                r: "{RING_RADIUS}",
            }
            circle {
                class: "activity-ring__value",
                cx: "9",
                cy: "9",
                r: "{RING_RADIUS}",
                stroke_dasharray: if offset.is_some() { "{circumference}" } else { "{circumference / 4.0} {circumference}" },
                stroke_dashoffset: "{offset.unwrap_or(0.0)}",
            }
        }
    }
}

/// Side panel listing running work and the notice history.
#[component]
pub fn ActivityPanel() -> Element {
    let state = use_context::<AppState>();
    let tasks = state.running_tasks();
    let notices = state.activity.notices.read().clone();
    let close = state.clone();
    let backdrop = state.clone();
    let clear = state.clone();
    rsx! {
        div {
            class: "queue-backdrop",
            onclick: move |_| backdrop.toggle_activity(),
        }
        aside {
            class: "queue-panel activity-panel",
            role: "dialog",
            aria_label: "Actividad",
            div {
                class: "queue-panel__header",
                span { class: "queue-panel__title", "Actividad" }
                div {
                    class: "activity-panel__actions",
                    if !notices.is_empty() {
                        button {
                            class: "btn-ghost activity-panel__clear",
                            onclick: move |_| clear.activity.clear_history(),
                            "Limpiar"
                        }
                    }
                    button {
                        class: "btn-icon-sm",
                        title: "Cerrar (Esc)",
                        aria_label: "Cerrar actividad",
                        onclick: move |_| close.toggle_activity(),
                        Icon { name: "close", size: 15 }
                    }
                }
            }
            div {
                class: "queue-panel__scroll",
                if !tasks.is_empty() {
                    div { class: "queue-label", "En curso" }
                    for task in tasks {
                        TaskRow { key: "{task.key}", task }
                    }
                }
                if !notices.is_empty() {
                    div { class: "queue-label", "Recientes" }
                    for notice in notices {
                        NoticeRow { key: "{notice.id}", notice }
                    }
                }
                if state.running_tasks().is_empty() && state.activity.notices.read().is_empty() {
                    EmptyState {
                        title: "Todo en calma",
                        hint: "Aquí verás las descargas, separaciones y demás tareas mientras trabajan, y los avisos recientes.",
                        icon: "bell",
                        compact: true,
                    }
                }
            }
        }
    }
}

#[component]
fn TaskRow(task: Task) -> Element {
    let state = use_context::<AppState>();
    let follow = state.clone();
    let page = task.page;
    let stop = task.stop.clone();
    rsx! {
        div {
            class: "task-row activity-task",
            span {
                class: "activity-task__icon",
                Icon { name: task.icon, size: 15 }
            }
            div {
                class: "task-row__body",
                if let Some(page) = page {
                    button {
                        class: "task-row__title activity-task__title",
                        title: "Abrir {page.label()}",
                        onclick: move |_| open_page(&follow, page),
                        "{task.title}"
                    }
                } else {
                    div { class: "task-row__title", "{task.title}" }
                }
                if !task.detail.is_empty() {
                    div { class: "task-row__status", "{task.detail}" }
                }
                div {
                    class: "progress-track",
                    role: "progressbar",
                    aria_label: "Progreso de {task.title}",
                    aria_valuemin: "0",
                    aria_valuemax: "100",
                    aria_valuenow: task.progress.map(|value| format!("{value:.0}")),
                    if let Some(progress) = task.progress {
                        div {
                            class: "progress-fill",
                            style: "width:{progress}%",
                        }
                    } else {
                        div { class: "progress-fill progress-fill--indeterminate" }
                    }
                }
            }
            if let Some(progress) = task.progress {
                span { class: "task-row__pct", "{progress:.0}%" }
            }
            if let Some(stop) = stop {
                button {
                    class: "btn-icon-sm",
                    title: "{stop.label()}",
                    aria_label: "{stop.label()} {task.title}",
                    onclick: move |_| state.stop_task(&stop),
                    Icon {
                        name: if matches!(stop, Stop::Job { label: "Pausar", .. }) { "pause" } else { "close" },
                        size: 13,
                    }
                }
            }
        }
    }
}

#[component]
fn NoticeRow(notice: Notice) -> Element {
    let state = use_context::<AppState>();
    rsx! {
        div {
            class: "activity-notice activity-notice--{notice.tone.class()}",
            Icon {
                name: notice.tone.icon(),
                size: 14,
                class: "activity-notice__icon",
            }
            div {
                class: "activity-notice__body",
                span { class: "activity-notice__text", "{notice.text}" }
                span {
                    class: "activity-notice__time",
                    "{notice.time}"
                    if notice.repeats > 0 {
                        " · ×{notice.repeats + 1}"
                    }
                }
            }
            if let Some(page) = notice.page {
                button {
                    class: "section-action",
                    onclick: move |_| open_page(&state, page),
                    "Ver"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{use_app_state, Services};
    use std::{cell::RefCell, rc::Rc};

    type Capture = Rc<RefCell<Option<AppState>>>;

    fn harness() -> Element {
        let state = use_app_state(use_context::<Rc<Services>>());
        *use_context::<Capture>().borrow_mut() = Some(state.clone());
        use_context_provider(|| state.clone());
        rsx! {
            Toasts {}
            ActivityButton {}
            if *state.activity.open.read() {
                ActivityPanel {}
            }
        }
    }

    #[tokio::test]
    async fn notices_render_as_toasts_then_move_to_the_panel_with_running_tasks() {
        let profile = tempfile::tempdir().unwrap();
        let capture: Capture = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(harness);
        dom.insert_any_root_context(Box::new(Rc::new(Services::for_test(profile.path()))));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let state = capture.borrow().as_ref().unwrap().clone();
        let task = dom.in_scope(ScopeId::ROOT, || {
            state.announce(
                Tone::Error,
                "No se pudo descargar «Prueba»",
                Some(Page::Downloads),
            );
            let task = state.begin_task("scissors", "Separando «Prueba»");
            task.progress(25.0, "Separando voz");
            task
        });
        dom.render_immediate_to_vec();
        let html = dioxus_ssr::render(&dom);
        assert!(html.contains("toast toast--error"), "{html}");
        assert!(html.contains("No se pudo descargar «Prueba»"));
        assert!(html.contains("role=\"alert\""));
        assert!(html.contains("class=\"activity-dot activity-dot--error\""));
        assert!(html.contains("Separando «Prueba» · 25%"));
        dom.in_scope(ScopeId::ROOT, || state.toggle_activity());
        dom.render_immediate_to_vec();
        let html = dioxus_ssr::render(&dom);
        assert!(!html.contains("toast toast--error"));
        assert!(html.contains("En curso"));
        assert!(html.contains("Recientes"));
        assert!(html.contains("Separando voz"));
        assert!(!html.contains("class=\"activity-dot"));
        drop(task);
        dom.render_immediate_to_vec();
        assert!(!dioxus_ssr::render(&dom).contains("En curso"));
    }
}
