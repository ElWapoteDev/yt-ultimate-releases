//! Shared test harness: an `AppState` inside a `VirtualDom` on a temporary
//! profile, without an audio device or network.

use crate::state::{use_app_state, AppState, Services};
use dioxus::prelude::*;
use std::{cell::RefCell, rc::Rc};
use yt_core::Track;

type Capture = Rc<RefCell<Option<AppState>>>;

pub(crate) struct TestApp {
    pub dom: VirtualDom,
    pub state: AppState,
    pub services: Rc<Services>,
    _profile: tempfile::TempDir,
}

impl TestApp {
    pub(crate) fn new() -> Self {
        Self::showing(|| rsx! { div {} })
    }

    /// The application rendering `body`.
    pub(crate) fn showing(body: fn() -> Element) -> Self {
        let profile = tempfile::tempdir().expect("temporary profile");
        let services = Rc::new(Services::for_test(profile.path()));
        let capture: Capture = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new_with_props(BodyHarness, BodyProps { body });
        dom.insert_any_root_context(Box::new(services.clone()));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let state = capture.borrow().clone().expect("captured state");
        Self {
            dom,
            state,
            services,
            _profile: profile,
        }
    }

    /// Run `action` inside the application scope, where signals are live.
    pub(crate) fn run<R>(&mut self, action: impl FnOnce(&mut AppState) -> R) -> R {
        let mut state = self.state.clone();
        self.dom.in_scope(ScopeId::ROOT, || action(&mut state))
    }

    /// Run `change` like `run`, then render the components it made dirty,
    /// as the desktop renderer does after each event.
    pub(crate) fn step(&mut self, change: impl FnOnce(&mut AppState)) {
        self.run(change);
        self.dom.render_immediate_to_vec();
    }

    /// A library track whose playback needs no audio device (video goes
    /// through the WebView element).
    pub(crate) fn video_track(&self, name: &str) -> Track {
        self.services
            .core
            .import_bytes(&format!("{name}.mp4"), name.as_bytes())
            .expect("imported track")
    }
}

pub(crate) fn remote_track(video_id: &str, title: &str) -> Track {
    crate::remote::track_from_result(
        yt_media::SearchResult {
            id: video_id.into(),
            url: format!("https://www.youtube.com/watch?v={video_id}"),
            title: title.into(),
            thumbnail: String::new(),
            duration_seconds: 200,
            channel: "Channel".into(),
            view_count: 0,
        },
        false,
    )
}

#[derive(Props, Clone)]
struct BodyProps {
    body: fn() -> Element,
}

// The harness renders one body for its whole life; props never change, and
// function pointers have no meaningful equality to compare.
impl PartialEq for BodyProps {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

#[allow(non_snake_case)]
fn BodyHarness(props: BodyProps) -> Element {
    let services = use_context::<Rc<Services>>();
    let capture = use_context::<Capture>();
    let state = use_app_state(services);
    *capture.borrow_mut() = Some(state.clone());
    use_context_provider(|| state);
    rsx! { {(props.body)()} }
}

/// Render `body` with application state prepared by `setup`, as HTML.
pub(crate) fn render(body: fn() -> Element, setup: impl FnOnce(&mut AppState)) -> String {
    let mut app = TestApp::showing(body);
    app.run(setup);
    app.dom.mark_dirty(ScopeId::ROOT);
    app.dom.render_immediate_to_vec();
    dioxus_ssr::render(&app.dom)
}
