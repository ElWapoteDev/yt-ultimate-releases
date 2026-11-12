use crate::{icons::Icon, state::AppState};
use dioxus::prelude::*;
use std::{
    cell::RefCell,
    fs::OpenOptions,
    io::Write,
    rc::Rc,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use yt_updater::{CancellationToken, Edition, InstallGuard, UpdateInfo, Updater, VerifiedUpdate};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Checking,
    Downloading,
    Ready,
    Installing,
    Failed,
}
impl Phase {
    fn busy(self) -> bool {
        matches!(self, Self::Checking | Self::Downloading | Self::Installing)
    }
}

#[derive(Clone)]
struct UpdateState {
    phase: Phase,
    available: Option<UpdateInfo>,
    ready: Option<VerifiedUpdate>,
    status: String,
    percent: u64,
    prompt: bool,
}

#[derive(Clone)]
struct Updates {
    app: AppState,
    updater: Result<Updater, String>,
    view: Signal<UpdateState>,
    cancel: Rc<RefCell<Option<CancellationToken>>>,
    progress: Arc<AtomicU64>,
    install_guard: Rc<RefCell<Option<InstallGuard>>>,
}

thread_local! { static ACTIVE: RefCell<Option<Updates>> = const { RefCell::new(None) }; }

/// The root owns this hook, so checks/downloads outlive the Settings screen.
pub(crate) fn use_updates(state: AppState) {
    let controller = use_controller(state);
    use_context_provider(|| controller.clone());
    let active = controller.clone();
    use_hook(move || ACTIVE.with(|value| *value.borrow_mut() = Some(active)));
    use_drop(|| {
        ACTIVE.with(|value| {
            value.borrow_mut().take();
        })
    });
}

fn use_controller(state: AppState) -> Updates {
    let updater = use_hook(|| {
        if crate::processing::REQUIRES_METAL {
            return Err(
                "Las actualizaciones de esta edición se instalan con su paquete Metal".into(),
            );
        }
        Updater::with_edition(
            env!("CARGO_PKG_VERSION"),
            state.services.profile.join("Updates"),
            if crate::processing::SEPARATOR_INCLUDED {
                Edition::Full
            } else {
                Edition::Base
            },
        )
        .map_err(|error| error.to_string())
    });
    let view = use_hook(|| {
        Signal::new_in_scope(
            UpdateState {
                phase: Phase::Idle,
                available: None,
                ready: None,
                status: previous_install_status(&state)
                    .or_else(|| updater.as_ref().err().cloned())
                    .unwrap_or_else(|| format!("Versión instalada: {}", env!("CARGO_PKG_VERSION"))),
                percent: 0,
                prompt: false,
            },
            ScopeId::ROOT,
        )
    });
    let cancel = use_hook(|| Rc::new(RefCell::new(None)));
    let progress = use_hook(|| Arc::new(AtomicU64::new(0)));
    let install_guard = use_hook(|| Rc::new(RefCell::new(None)));
    let controller = Updates {
        app: state,
        updater,
        view,
        cancel,
        progress,
        install_guard,
    };
    let poll = controller.clone();
    use_hook(move || {
        spawn(async move {
            let mut next_check = Instant::now() + Duration::from_secs(20);
            loop {
                tokio::time::sleep(Duration::from_millis(250)).await;
                if poll.view.peek().phase == Phase::Downloading {
                    let value = poll.progress.load(Ordering::Relaxed);
                    if poll.view.peek().percent != value {
                        poll.edit(|view| view.percent = value);
                    }
                }
                if Instant::now() >= next_check {
                    next_check = Instant::now() + Duration::from_secs(6 * 60 * 60);
                    if preference(&poll.app, "update_auto_check", true)
                        && !poll.view.peek().phase.busy()
                    {
                        poll.check(false);
                    }
                }
            }
        })
    });
    controller
}

fn preference(state: &AppState, key: &str, default: bool) -> bool {
    state
        .settings
        .peek()
        .extra
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(default)
}

fn previous_install_status(state: &AppState) -> Option<String> {
    let path = state.services.profile.join("Updates/last-install.json");
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 16 * 1024 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    let message = value.get("message")?.as_str()?.to_owned();
    let _ = std::fs::remove_file(path);
    Some(message)
}

impl Updates {
    fn edit(&self, change: impl FnOnce(&mut UpdateState)) {
        let mut view = self.view;
        change(&mut view.write());
    }
    fn log(&self, event: &str) {
        let directory = self.app.services.profile.join("Updates");
        let _ = std::fs::create_dir_all(&directory);
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join("updater.log"))
        {
            let _ = writeln!(file, "{} {event}", chrono::Utc::now().to_rfc3339());
        }
    }
    fn failure(&self, message: String) {
        self.log("operation-failed");
        self.edit(|view| {
            view.phase = Phase::Failed;
            view.status = message.clone();
        });
        self.app.notify_error(message);
    }
    fn check(&self, interactive: bool) {
        if self.view.peek().phase.busy() {
            if interactive {
                self.app.notify("La actualización ya está en curso");
            }
            return;
        }
        if self.view.peek().ready.is_some() {
            if interactive {
                self.edit(|view| view.prompt = true);
            }
            return;
        }
        let Ok(updater) = self.updater.clone() else {
            return;
        };
        let token = CancellationToken::new();
        *self.cancel.borrow_mut() = Some(token.clone());
        self.edit(|view| {
            view.phase = Phase::Checking;
            view.status = "Buscando actualizaciones…".into();
        });
        self.log("checking");
        let this = self.clone();
        dioxus::core::spawn_forever(async move {
            let result = tokio::select! { biased; _=token.cancelled()=>Err(yt_updater::UpdateError::Canceled), result=updater.check()=>result };
            this.cancel.borrow_mut().take();
            match result {
                Ok(Some(info)) => {
                    this.log("update-available");
                    this.edit(|view| {
                        view.phase = Phase::Idle;
                        view.status = format!("Versión {} disponible", info.version);
                        view.available = Some(info);
                    });
                    if preference(&this.app, "update_auto_download", true) {
                        this.download();
                    }
                }
                Ok(None) => this.edit(|view| {
                    view.phase = Phase::Idle;
                    view.status = "Tienes la última versión disponible para esta edición".into();
                    view.available = None;
                }),
                Err(yt_updater::UpdateError::Canceled) => this.edit(|view| {
                    view.phase = Phase::Idle;
                    view.status = "Búsqueda cancelada".into();
                }),
                Err(error) => {
                    this.log("check-failed");
                    this.edit(|view| {
                        view.phase = Phase::Failed;
                        view.status = error.to_string();
                    });
                    if interactive {
                        this.app.notify_error(error.to_string());
                    }
                }
            }
        });
    }
    fn download(&self) {
        if self.view.peek().phase.busy() {
            return;
        }
        let Some(info) = self.view.peek().available.clone() else {
            return;
        };
        let Ok(updater) = self.updater.clone() else {
            return;
        };
        let token = CancellationToken::new();
        *self.cancel.borrow_mut() = Some(token.clone());
        self.progress.store(0, Ordering::Relaxed);
        self.edit(|view| {
            view.phase = Phase::Downloading;
            view.percent = 0;
            view.status = "Descargando actualización…".into();
        });
        self.log("downloading");
        let this = self.clone();
        dioxus::core::spawn_forever(async move {
            let progress = this.progress.clone();
            let result = updater
                .download(&info, token, move |p| {
                    progress.store(
                        p.downloaded_bytes
                            .saturating_mul(100)
                            .checked_div(p.total_bytes)
                            .unwrap_or(0),
                        Ordering::Relaxed,
                    );
                })
                .await;
            this.cancel.borrow_mut().take();
            match result {
                Ok(update) => {
                    this.log("download-verified");
                    this.edit(|view| {
                        view.phase = Phase::Ready;
                        view.percent = 100;
                        view.status = format!(
                            "La versión {} está lista para instalar",
                            update.info().version
                        );
                        view.ready = Some(update);
                        view.prompt = true;
                    });
                }
                Err(yt_updater::UpdateError::Canceled) => this.edit(|view| {
                    view.phase = Phase::Idle;
                    view.status = "Descarga cancelada".into();
                }),
                Err(error) => this.failure(error.to_string()),
            }
        });
    }
    fn install(&self, restart: bool) -> bool {
        if self.view.peek().phase == Phase::Installing {
            return true;
        }
        let Some(update) = self.view.peek().ready.clone() else {
            return false;
        };
        let Ok(updater) = self.updater.clone() else {
            return false;
        };
        self.edit(|view| {
            view.phase = Phase::Installing;
            view.prompt = false;
            view.status = "Guardando y esperando a que terminen los trabajos…".into();
        });
        self.log("installation-requested");
        self.app.persist_playback();
        self.app.cancel_remote();
        for token in self.app.cancellations.peek().values() {
            token.cancel();
        }
        let this = self.clone();
        dioxus::core::spawn_forever(async move {
            let deadline = Instant::now() + Duration::from_secs(120);
            while !this.app.cancellations.peek().is_empty() || *this.app.busy.peek() {
                if Instant::now() >= deadline {
                    this.failure("Hay trabajos que todavía no terminaron. La actualización no se instaló; vuelve a intentarlo cuando finalicen.".into());
                    crate::native::restore_after_update(&this.app);
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            this.edit(|view| view.status = "Comprobando la actualización antes de cerrar…".into());
            let result =
                tokio::task::spawn_blocking(move || updater.handoff_install(&update, restart))
                    .await;
            match result {
                Ok(Ok(guard)) => {
                    if !this.app.cancellations.peek().is_empty() || *this.app.busy.peek() {
                        guard.abort();
                        this.failure("Empezó otro trabajo mientras se verificaba la actualización. Espera a que termine y vuelve a instalar.".into());
                        crate::native::restore_after_update(&this.app);
                        return;
                    }
                    *this.install_guard.borrow_mut() = Some(guard);
                    this.log("installer-handoff-ready");
                    crate::native::finish_close(&this.app);
                }
                Ok(Err(error)) => {
                    this.failure(error.to_string());
                    crate::native::restore_after_update(&this.app);
                }
                Err(error) => {
                    this.failure(error.to_string());
                    crate::native::restore_after_update(&this.app);
                }
            }
        });
        true
    }
}

/// The updater's current work as an activity task. Reading it subscribes the
/// calling component to the root controller's state.
pub(crate) fn activity_task() -> Option<crate::activity::Task> {
    let view = ACTIVE.with(|active| active.borrow().as_ref().map(|controller| controller.view))?;
    let view = view.read();
    let (title, progress) = match view.phase {
        Phase::Checking => ("Buscando actualizaciones", None),
        Phase::Downloading => ("Descargando actualización", Some(view.percent as f64)),
        Phase::Installing => ("Instalando actualización", None),
        Phase::Idle | Phase::Ready | Phase::Failed => return None,
    };
    Some(crate::activity::Task {
        key: "update".into(),
        icon: "download-cloud",
        title: title.into(),
        detail: view
            .available
            .as_ref()
            .map(|info| format!("Versión {}", info.version))
            .unwrap_or_default(),
        progress,
        stop: matches!(view.phase, Phase::Checking | Phase::Downloading)
            .then_some(crate::activity::Stop::Update),
        page: Some(crate::state::Page::Settings),
    })
}

/// Cancel the update check or download in progress, if any.
pub(crate) fn cancel_active() {
    ACTIVE.with(|active| {
        if let Some(token) = active
            .borrow()
            .as_ref()
            .and_then(|controller| controller.cancel.borrow().clone())
        {
            token.cancel();
        }
    });
}

/// Native close handlers hide rather than destroy the window while this returns
/// true. Installation on exit is enabled only by an explicitly saved preference.
pub(crate) fn request_exit(state: &AppState) -> bool {
    ACTIVE.with(|active| {
        let Some(controller) = active.borrow().clone() else {
            return false;
        };
        if controller.view.peek().phase == Phase::Installing {
            return true;
        }
        if preference(state, "update_install_on_exit", false)
            && controller.view.peek().ready.is_some()
        {
            return controller.install(false);
        }
        if let Some(token) = controller.cancel.borrow().as_ref() {
            token.cancel();
        }
        false
    })
}

#[component]
pub fn UpdatePanel() -> Element {
    let state = use_context::<AppState>();
    // Standalone previews/tests can mount Settings without the desktop root.
    let updates = if let Some(value) = try_consume_context::<Updates>() {
        value
    } else {
        use_controller(state.clone())
    };
    let view = updates.view.read().clone();
    let check = updates.clone();
    let download = updates.clone();
    let install = updates.clone();
    let cancel = updates.clone();
    let logs = state.clone();
    let manual = updates.clone();
    rsx! {section{class:"settings-section",
      div{class:"settings-section__header",div{class:"settings-section__icon",Icon{name:"download",size:19}}div{div{class:"settings-section__title","Actualizaciones"}div{class:"settings-section__sub","Nuevas versiones de YT Ultimate"}}}
      p{class:"rust-settings-note",role:"status","{view.status}"}
      if let Err(error)=&updates.updater {p{class:"rust-settings-note rust-danger","{error}"}}
      UpdatePreference{key_name:"update_auto_check",title:"Buscar automáticamente",description:"Al abrir la aplicación y cada seis horas",default:true}
      UpdatePreference{key_name:"update_auto_download",title:"Descargar automáticamente",description:"Descarga y verifica la nueva versión en segundo plano",default:true}
      UpdatePreference{key_name:"update_install_on_exit",title:"Instalar al salir",description:"Aplica la actualización verificada cuando cierres la aplicación",default:false}
      div{style:"display:flex;gap:8px;flex-wrap:wrap;padding:12px",
        button{class:"btn-secondary",disabled:view.phase.busy()||updates.updater.is_err(),onclick:move |_|check.check(true),"Buscar actualizaciones"}
        if view.available.is_some() && view.ready.is_none(){button{class:"btn-accent",disabled:view.phase.busy(),onclick:move |_|download.download(),"Descargar actualización"}}
        if view.ready.is_some(){button{class:"btn-accent",disabled:view.phase.busy(),onclick:move |_|{install.install(true);},"Reiniciar e instalar"}
          button{class:"btn-secondary",disabled:view.phase.busy(),onclick:move |_|{let this=manual.clone();let ready=this.view.peek().ready.clone();if let (Some(update),Ok(updater))=(ready,this.updater.clone()){dioxus::core::spawn_forever(async move {match tokio::task::spawn_blocking(move||updater.open_installer(&update)).await{Ok(Ok(()))=>this.app.notify_success("Instalador abierto"),Ok(Err(e))=>this.failure(e.to_string()),Err(e)=>this.failure(e.to_string())}});}},"Abrir instalador"}}
        if matches!(view.phase,Phase::Checking|Phase::Downloading){button{class:"btn-secondary",onclick:move |_|{if let Some(token)=cancel.cancel.borrow().as_ref(){token.cancel();}},"Cancelar"}}
        button{class:"btn-secondary",onclick:move |_|crate::native::open_folder(&logs,&logs.services.profile.join("Updates")),"Abrir logs"}
      }
      if view.phase==Phase::Downloading {progress{class:"rust-progress",max:"100",value:"{view.percent}"}span{class:"rust-settings-note","{view.percent}%"}}
    }}
}

#[component]
fn UpdatePreference(
    key_name: String,
    title: String,
    description: String,
    default: bool,
) -> Element {
    let state = use_context::<AppState>();
    let checked = state
        .settings
        .read()
        .extra
        .get(&key_name)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(default);
    rsx! {button{class:"settings-row settings-row--toggle",role:"switch",aria_checked:checked,onclick:move |_|state.update_setting(&key_name,serde_json::json!(!checked)),div{class:"settings-row__label",span{class:"settings-row__title","{title}"}span{class:"settings-row__sub","{description}"}}div{class:if checked{"settings-switch active"}else{"settings-switch"},div{class:""}}}}
}

/// Render once near the root modals, so the downloaded notification is not tied
/// to whether Settings is mounted.
#[component]
pub(crate) fn UpdatePrompt() -> Element {
    let updates = use_context::<Updates>();
    let view = updates.view.read().clone();
    if view.phase == Phase::Installing {
        return rsx! {div{class:"modal-backdrop",div{class:"modal-panel",role:"dialog","aria-modal":"true",h2{"Instalando actualización"}p{"{view.status}"}}}};
    }
    if !view.prompt || view.ready.is_none() {
        return rsx! {};
    }
    let version = view
        .ready
        .as_ref()
        .map(|u| u.info().version.clone())
        .unwrap_or_default();
    let install = updates.clone();
    let later = updates.clone();
    rsx! {div{class:"modal-backdrop",div{class:"modal-panel",role:"dialog","aria-modal":"true",h2{"Actualización lista"}p{"YT Ultimate {version} está descargado y verificado."}p{if preference(&updates.app,"update_install_on_exit",false){"Si eliges Más tarde, se instalará cuando cierres la aplicación."}else{"Puedes instalarla ahora o más tarde desde Ajustes."}}div{class:"modal-actions",button{class:"btn-secondary",onclick:move |_|later.edit(|view|view.prompt=false),"Más tarde"}button{class:"btn-accent",onclick:move |_|{install.install(true);},"Reiniciar ahora"}}}}}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{use_app_state, Page, Services};
    type Capture = Rc<RefCell<Option<(AppState, Updates)>>>;

    fn harness() -> Element {
        let services = use_context::<Rc<Services>>();
        let capture = use_context::<Capture>();
        let state = use_app_state(services);
        use_context_provider(|| state.clone());
        use_updates(state.clone());
        let updates = use_context::<Updates>();
        *capture.borrow_mut() = Some((state.clone(), updates));
        let settings = *state.page.read() == Page::Settings;
        rsx! {if settings{UpdatePanel{}}else{div{"Otra página"}}UpdatePrompt{}}
    }

    #[tokio::test]
    async fn update_state_and_cancellation_survive_settings_unmount_and_preferences_are_explicit() {
        let directory = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(directory.path()));
        let capture: Capture = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(harness);
        dom.insert_any_root_context(Box::new(services.clone()));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let (mut state, updates) = capture.borrow().as_ref().unwrap().clone();
        assert!(!preference(&state, "update_install_on_exit", false));
        assert!(preference(&state, "update_auto_check", true));
        let token = CancellationToken::new();
        *updates.cancel.borrow_mut() = Some(token.clone());
        updates.edit(|view| {
            view.phase = Phase::Downloading;
            view.percent = 43;
            view.status = "Descarga persistente".into();
        });
        state.page.set(Page::Settings);
        dom.render_immediate_to_vec();
        assert!(dioxus_ssr::render(&dom).contains("Descarga persistente"));
        state.page.set(Page::Home);
        dom.render_immediate_to_vec();
        updates.edit(|view| view.percent = 71);
        state.page.set(Page::Settings);
        dom.render_immediate_to_vec();
        assert!(dioxus_ssr::render(&dom).contains("71%"));
        assert!(!request_exit(&state));
        assert!(token.is_cancelled());
        state.update_setting("update_install_on_exit", serde_json::json!(true));
        assert_eq!(
            services.core.settings().unwrap().extra["update_install_on_exit"],
            true
        );
        assert!(!request_exit(&state), "no downloaded installer exists");
    }

    #[tokio::test]
    async fn previous_install_failure_is_shown_once_without_opening_an_installer() {
        let directory = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(directory.path()));
        std::fs::create_dir_all(directory.path().join("Updates")).unwrap();
        let result = directory.path().join("Updates/last-install.json");
        std::fs::write(
            &result,
            r#"{"message":"No se pudo instalar: permiso denegado","success":false}"#,
        )
        .unwrap();
        let capture: Capture = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(harness);
        dom.insert_any_root_context(Box::new(services));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let (_, updates) = capture.borrow().as_ref().unwrap().clone();
        assert!(updates.view.peek().status.contains("permiso denegado"));
        assert!(!result.exists());
        assert!(updates.install_guard.borrow().is_none());
    }
}
