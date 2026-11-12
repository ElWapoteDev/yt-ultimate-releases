//! Activity: what the application is doing and what it has told the user.
//! Long work registers a task while it runs; every outcome becomes a notice with
//! a tone, shown as a toast and kept in a bounded history.
use crate::state::{AppState, DownloadView, Page};
use dioxus::prelude::*;
use std::time::{Duration, Instant};

pub const NOTICE_HISTORY: usize = 60;
pub const VISIBLE_TOASTS: usize = 3;
/// Time a toast stays after the pointer leaves the stack.
const HOVER_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Info,
    Success,
    Warning,
    Error,
}

impl Tone {
    pub fn icon(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Success => "check-circle",
            Self::Warning => "alert-triangle",
            Self::Error => "alert-circle",
        }
    }
    pub fn class(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
    fn lifetime(self) -> Duration {
        Duration::from_millis(match self {
            Self::Info | Self::Success => 4_500,
            Self::Warning => 7_000,
            Self::Error => 10_000,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub id: u64,
    pub tone: Tone,
    pub text: String,
    /// Local wall-clock time, formatted once when the notice was recorded.
    pub time: String,
    pub page: Option<Page>,
    /// Identical notices received while the first was still on screen.
    pub repeats: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Toast {
    pub id: u64,
    expires: Instant,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Unread {
    pub count: usize,
    pub errors: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stop {
    /// Cancels the token registered under `key` in `AppState::cancellations`.
    Job { key: String, label: &'static str },
    /// Cancels the update check or download in progress.
    Update,
    /// Cancels the media being opened for playback.
    Playback,
}

impl Stop {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Job { label, .. } => label,
            Self::Update | Self::Playback => "Cancelar",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub key: String,
    pub icon: &'static str,
    pub title: String,
    pub detail: String,
    /// Percent complete; `None` while the amount of work is unknown.
    pub progress: Option<f64>,
    pub stop: Option<Stop>,
    pub page: Option<Page>,
}

#[derive(Clone, Copy)]
pub struct Activity {
    pub notices: Signal<Vec<Notice>>,
    pub toasts: Signal<Vec<Toast>>,
    pub tasks: Signal<Vec<Task>>,
    pub unread: Signal<Unread>,
    pub open: Signal<bool>,
    hovered: Signal<bool>,
    sequence: Signal<u64>,
}

impl Activity {
    /// Hook: create the activity signals with the application's lifetime.
    pub fn use_new(startup_error: Option<String>) -> Self {
        let now = Instant::now();
        let startup = startup_error.map(|text| Notice {
            id: 1,
            tone: Tone::Error,
            text,
            time: clock(),
            page: None,
            repeats: 0,
        });
        let toasts = startup
            .iter()
            .map(|notice| Toast {
                id: notice.id,
                expires: now + notice.tone.lifetime(),
            })
            .collect::<Vec<_>>();
        let unread = Unread {
            count: startup.iter().count(),
            errors: startup.iter().count(),
        };
        Self {
            notices: crate::state::use_app_signal(|| startup.into_iter().collect()),
            toasts: crate::state::use_app_signal(|| toasts),
            tasks: crate::state::use_app_signal(Vec::new),
            unread: crate::state::use_app_signal(|| unread),
            open: crate::state::use_app_signal(|| false),
            hovered: crate::state::use_app_signal(|| false),
            sequence: crate::state::use_app_signal(|| 1),
        }
    }

    fn next_id(&self) -> u64 {
        let mut sequence = self.sequence;
        let id = *sequence.peek() + 1;
        sequence.set(id);
        id
    }

    fn record(&self, tone: Tone, text: String, page: Option<Page>) {
        let notice = Notice {
            id: self.next_id(),
            tone,
            text,
            time: clock(),
            page,
            repeats: 0,
        };
        let open = *self.open.peek();
        let (mut notices, mut toasts) = (self.notices, self.toasts);
        let fresh = remember(
            &mut notices.write(),
            &mut toasts.write(),
            notice,
            !open,
            Instant::now(),
        );
        if fresh && !open {
            let mut unread = self.unread;
            let mut count = unread.write();
            count.count += 1;
            count.errors += usize::from(tone == Tone::Error);
        }
    }

    pub fn dismiss(&self, id: u64) {
        let mut toasts = self.toasts;
        toasts.write().retain(|toast| toast.id != id);
    }

    /// Retire expired toasts unless the pointer is resting on the stack. Driven by
    /// the shared playback poll, so toasts need no timer of their own.
    pub fn expire(&self, now: Instant) {
        if *self.hovered.peek() || self.toasts.peek().is_empty() {
            return;
        }
        if self.toasts.peek().iter().any(|toast| toast.expires <= now) {
            let mut toasts = self.toasts;
            toasts.write().retain(|toast| toast.expires > now);
        }
    }

    pub fn hover(&self, hovered: bool) {
        let mut flag = self.hovered;
        flag.set(hovered);
        if !hovered {
            let deadline = Instant::now() + HOVER_GRACE;
            let mut toasts = self.toasts;
            for toast in toasts.write().iter_mut() {
                toast.expires = toast.expires.max(deadline);
            }
        }
    }

    pub fn clear_history(&self) {
        let (mut notices, mut toasts, mut unread) = (self.notices, self.toasts, self.unread);
        notices.write().clear();
        toasts.write().clear();
        unread.set(Unread::default());
    }

    fn begin(&self, icon: &'static str, title: String) -> TaskHandle {
        let key = format!("task:{}", self.next_id());
        let mut tasks = self.tasks;
        tasks.write().push(Task {
            key: key.clone(),
            icon,
            title,
            detail: String::new(),
            progress: None,
            stop: None,
            page: None,
        });
        TaskHandle {
            tasks: self.tasks,
            key,
        }
    }
}

/// Add a notice to the history and, when `show` is set, to the toast stack. A
/// notice identical to one still on screen refreshes it instead of stacking.
/// Returns whether the notice is new.
fn remember(
    history: &mut Vec<Notice>,
    toasts: &mut Vec<Toast>,
    notice: Notice,
    show: bool,
    now: Instant,
) -> bool {
    let expires = now + notice.tone.lifetime();
    let repeated = history.iter().position(|old| {
        old.tone == notice.tone
            && old.text == notice.text
            && toasts.iter().any(|toast| toast.id == old.id)
    });
    if let Some(index) = repeated {
        let mut old = history.remove(index);
        old.repeats += 1;
        old.time = notice.time;
        toasts.retain(|toast| toast.id != old.id);
        toasts.push(Toast {
            id: old.id,
            expires,
        });
        history.insert(0, old);
        return false;
    }
    if show {
        toasts.push(Toast {
            id: notice.id,
            expires,
        });
        if toasts.len() > VISIBLE_TOASTS {
            toasts.remove(0);
        }
    }
    history.insert(0, notice);
    history.truncate(NOTICE_HISTORY);
    true
}

fn clock() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

/// A running task in the activity registry; dropping the handle ends it, so a
/// task never outlives the work that registered it, whatever path that work takes.
#[must_use = "the task ends when its handle is dropped"]
pub struct TaskHandle {
    tasks: Signal<Vec<Task>>,
    key: String,
}

impl TaskHandle {
    pub fn page(self, page: Page) -> Self {
        self.edit(|task| task.page = Some(page));
        self
    }

    pub fn stop(self, stop: Stop) -> Self {
        self.edit(|task| task.stop = Some(stop));
        self
    }

    pub fn progress(&self, percent: f64, detail: impl Into<String>) {
        let percent = percent.is_finite().then(|| percent.clamp(0.0, 100.0));
        let detail = detail.into();
        self.edit(|task| {
            task.progress = percent;
            task.detail = detail;
        });
    }

    pub fn detail(&self, detail: impl Into<String>) {
        let detail = detail.into();
        self.edit(|task| task.detail = detail);
    }

    // Writes only when the visible task changes, so progress callbacks that
    // repeat a value never re-render the activity views.
    fn edit(&self, change: impl FnOnce(&mut Task)) {
        let Some(mut task) = self
            .tasks
            .try_peek()
            .ok()
            .and_then(|tasks| tasks.iter().find(|task| task.key == self.key).cloned())
        else {
            return;
        };
        let before = task.clone();
        change(&mut task);
        if task != before {
            let mut tasks = self.tasks;
            if let Ok(mut list) = tasks.try_write() {
                if let Some(slot) = list.iter_mut().find(|slot| slot.key == self.key) {
                    *slot = task;
                }
            };
        }
    }
}

impl Drop for TaskHandle {
    fn drop(&mut self) {
        if let Ok(mut tasks) = self.tasks.try_write() {
            tasks.retain(|task| task.key != self.key);
        }
    }
}

fn download_task(item: &DownloadView) -> Task {
    let (label, _, waiting) = crate::downloads_ui::phase(item);
    Task {
        key: format!("download:{}", item.id),
        icon: if item.video { "video" } else { "download" },
        title: item.title.clone(),
        detail: if item.detail.is_empty() {
            label.to_owned()
        } else {
            item.detail.clone()
        },
        progress: (!waiting && item.progress > 0.0).then(|| item.progress.clamp(0.0, 100.0)),
        stop: Some(Stop::Job {
            key: item.id.clone(),
            label: "Pausar",
        }),
        page: Some(Page::Downloads),
    }
}

/// Mean progress of the running tasks, or `None` when any of them is unknown.
pub fn overall_progress(tasks: &[Task]) -> Option<f64> {
    let known = tasks
        .iter()
        .map(|task| task.progress)
        .collect::<Option<Vec<_>>>()?;
    (!known.is_empty()).then(|| known.iter().sum::<f64>() / known.len() as f64)
}

impl AppState {
    pub fn notify(&self, text: impl Into<String>) {
        self.announce(Tone::Info, text, None);
    }

    pub fn notify_success(&self, text: impl Into<String>) {
        self.announce(Tone::Success, text, None);
    }

    pub fn notify_warning(&self, text: impl Into<String>) {
        self.announce(Tone::Warning, text, None);
    }

    pub fn notify_error(&self, text: impl Into<String>) {
        self.announce(Tone::Error, text, None);
    }

    /// Record a notice; `page` adds a «Ver» action that opens where it happened.
    pub fn announce(&self, tone: Tone, text: impl Into<String>, page: Option<Page>) {
        let text = text.into();
        crate::native::record_log(&self.services.profile, &format!("{}: {text}", tone.class()));
        self.activity.record(tone, text, page);
    }

    pub fn begin_task(&self, icon: &'static str, title: impl Into<String>) -> TaskHandle {
        self.activity.begin(icon, title.into())
    }

    /// Every running task: registered work, active downloads and the updater.
    pub fn running_tasks(&self) -> Vec<Task> {
        let mut tasks = self.activity.tasks.read().clone();
        tasks.extend(
            self.downloads
                .read()
                .iter()
                .filter(|item| crate::downloads_ui::active(item))
                .map(download_task),
        );
        tasks.extend(crate::updates::activity_task());
        tasks
    }

    pub fn stop_task(&self, stop: &Stop) {
        match stop {
            Stop::Job { key, .. } => {
                if let Some(token) = self.cancellations.peek().get(key) {
                    token.cancel();
                }
            }
            Stop::Update => crate::updates::cancel_active(),
            Stop::Playback => self.cancel_remote(),
        }
    }

    pub fn toggle_activity(&self) {
        let mut open = self.activity.open;
        let next = !*open.peek();
        open.set(next);
        if next {
            let (mut queue, mut unread, mut toasts) =
                (self.queue_open, self.activity.unread, self.activity.toasts);
            queue.set(false);
            unread.set(Unread::default());
            toasts.write().clear();
        }
    }
}

#[cfg(test)]
impl AppState {
    /// Text of the newest notice, for assertions in component tests.
    pub fn last_notice(&self) -> Option<String> {
        self.activity
            .notices
            .peek()
            .first()
            .map(|notice| notice.text.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{use_app_state, Services};
    use std::{cell::RefCell, rc::Rc};

    fn notice(id: u64, tone: Tone, text: &str) -> Notice {
        Notice {
            id,
            tone,
            text: text.into(),
            time: "12:00".into(),
            page: None,
            repeats: 0,
        }
    }

    #[test]
    fn history_is_bounded_newest_first_and_toasts_keep_the_latest_three() {
        let (mut history, mut toasts, now) = (Vec::new(), Vec::new(), Instant::now());
        for id in 1..=(NOTICE_HISTORY as u64 + 5) {
            assert!(remember(
                &mut history,
                &mut toasts,
                notice(id, Tone::Info, &format!("aviso {id}")),
                true,
                now,
            ));
        }
        assert_eq!(history.len(), NOTICE_HISTORY);
        assert_eq!(history[0].id, NOTICE_HISTORY as u64 + 5);
        assert_eq!(
            toasts.iter().map(|toast| toast.id).collect::<Vec<_>>(),
            vec![
                NOTICE_HISTORY as u64 + 3,
                NOTICE_HISTORY as u64 + 4,
                NOTICE_HISTORY as u64 + 5
            ]
        );
    }

    #[test]
    fn repeated_visible_notices_refresh_instead_of_stacking() {
        let (mut history, mut toasts, now) = (Vec::new(), Vec::new(), Instant::now());
        remember(
            &mut history,
            &mut toasts,
            notice(1, Tone::Warning, "En cola"),
            true,
            now,
        );
        remember(
            &mut history,
            &mut toasts,
            notice(2, Tone::Info, "Otro"),
            true,
            now,
        );
        let later = now + Duration::from_secs(1);
        assert!(!remember(
            &mut history,
            &mut toasts,
            notice(3, Tone::Warning, "En cola"),
            true,
            later,
        ));
        assert_eq!(history.len(), 2);
        assert_eq!((history[0].id, history[0].repeats), (1, 1));
        assert_eq!(toasts.last().unwrap().id, 1);
        assert_eq!(toasts.len(), 2);
        toasts.clear();
        assert!(remember(
            &mut history,
            &mut toasts,
            notice(4, Tone::Warning, "En cola"),
            true,
            later,
        ));
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn errors_outlive_information_and_hidden_notices_stay_in_history_only() {
        let (mut history, mut toasts, now) = (Vec::new(), Vec::new(), Instant::now());
        remember(
            &mut history,
            &mut toasts,
            notice(1, Tone::Success, "Listo"),
            true,
            now,
        );
        remember(
            &mut history,
            &mut toasts,
            notice(2, Tone::Error, "Falló"),
            true,
            now,
        );
        remember(
            &mut history,
            &mut toasts,
            notice(3, Tone::Info, "Oculto"),
            false,
            now,
        );
        assert_eq!(history.len(), 3);
        let after_info = now + Tone::Info.lifetime() + Duration::from_millis(1);
        toasts.retain(|toast| toast.expires > after_info);
        assert_eq!(
            toasts.iter().map(|toast| toast.id).collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn overall_progress_is_unknown_when_any_task_is() {
        let task = |progress| Task {
            key: String::new(),
            icon: "music",
            title: String::new(),
            detail: String::new(),
            progress,
            stop: None,
            page: None,
        };
        assert_eq!(
            overall_progress(&[task(Some(20.0)), task(Some(60.0))]),
            Some(40.0)
        );
        assert_eq!(overall_progress(&[task(Some(20.0)), task(None)]), None);
        assert_eq!(overall_progress(&[]), None);
    }

    type Capture = Rc<RefCell<Option<AppState>>>;

    fn harness() -> Element {
        let state = use_app_state(use_context::<Rc<Services>>());
        *use_context::<Capture>().borrow_mut() = Some(state.clone());
        use_context_provider(|| state);
        rsx! {}
    }

    #[tokio::test]
    async fn tasks_end_with_their_handle_and_the_panel_marks_notices_read() {
        let profile = tempfile::tempdir().unwrap();
        let capture: Capture = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(harness);
        dom.insert_any_root_context(Box::new(Rc::new(Services::for_test(profile.path()))));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let state = capture.borrow().as_ref().unwrap().clone();
        dom.in_scope(ScopeId::ROOT, || {
            let task = state.begin_task("scissors", "Separando").page(Page::Stems);
            task.progress(41.6, "Separando voz");
            task.progress(41.6, "Separando voz");
            let running = state.running_tasks();
            assert_eq!(running.len(), 1);
            assert_eq!(running[0].progress, Some(41.6));
            assert_eq!(running[0].page, Some(Page::Stems));
            drop(task);
            assert!(state.running_tasks().is_empty());

            state.notify_error("No se pudo descargar");
            state.notify_success("Descarga completada");
            assert_eq!(
                *state.activity.unread.peek(),
                Unread {
                    count: 2,
                    errors: 1
                }
            );
            assert_eq!(state.activity.toasts.peek().len(), 2);
            state.toggle_activity();
            assert!(*state.activity.open.peek());
            assert_eq!(*state.activity.unread.peek(), Unread::default());
            assert!(state.activity.toasts.peek().is_empty());
            state.notify("Mientras el panel está abierto");
            assert!(state.activity.toasts.peek().is_empty());
            assert_eq!(state.activity.notices.peek().len(), 3);
            assert_eq!(state.activity.notices.peek()[1].tone, Tone::Success);
        });
    }
}
