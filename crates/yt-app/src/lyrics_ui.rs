//! Lyrics views: the player-bar button, the side panel, the full-screen
//! lyrics stage and their karaoke lines. The interface text of the feature
//! lives here. See `docs/systems/Lyrics.md`.

use crate::{
    icons::Icon,
    lyrics::{lyrics_clock_ms, LyricsView, ShownLyrics, OFFSET_STEP_MS},
    state::AppState,
};
use dioxus::{
    html::{geometry::PixelsVector2D, ScrollBehavior},
    prelude::*,
};
use std::{
    rc::Rc,
    time::{Duration, Instant},
};
use yt_core::LyricsSource;
use yt_lyrics::{active_line, line_segments, KaraokeConfig};

/// After the user scrolls the lyrics, the view waits this long before it
/// follows the sung line again.
const FOLLOW_PAUSE: Duration = Duration::from_secs(4);
/// Where the middle of the sung line sits in the lyrics list, as a fraction
/// of the list's height from its top: above the middle, so more of what comes
/// next shows.
const FOCUS_POINT: f64 = 0.4;
/// Fixed script asking the WebView whether the user prefers reduced motion.
const REDUCED_MOTION_QUERY: &str =
    "return window.matchMedia('(prefers-reduced-motion: reduce)').matches;";

fn source_label(source: LyricsSource) -> &'static str {
    match source {
        LyricsSource::Sidecar => "Archivo .lrc",
        LyricsSource::Embedded => "Etiquetas del archivo",
        LyricsSource::Lrclib => "LRCLIB",
    }
}

/// «+1,5 s» with a decimal comma; «Sincronía» when there is no offset.
fn offset_label(offset_ms: i32) -> String {
    if offset_ms == 0 {
        return "Sincronía".into();
    }
    let sign = if offset_ms > 0 { '+' } else { '−' };
    let tenths = offset_ms.unsigned_abs().div_ceil(100);
    format!("{sign}{},{} s", tenths / 10, tenths % 10)
}

/// Lyrics shared with rows, compared by identity so a render never compares
/// whole songs line by line.
#[derive(Clone)]
struct SharedLyrics(Rc<ShownLyrics>);

impl PartialEq for SharedLyrics {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

/// Where a line is relative to what is sung.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineKind {
    Past,
    Active,
    /// The line after the active one; its fill is scheduled ahead so it
    /// starts on time.
    Next,
    Upcoming,
}

impl LineKind {
    fn class(self) -> &'static str {
        match self {
            Self::Past => "lyric-line lyric-line--past",
            Self::Active => "lyric-line lyric-line--active",
            Self::Next => "lyric-line lyric-line--next",
            Self::Upcoming => "lyric-line lyric-line--upcoming",
        }
    }
}

/// Kind of every line: the active line and those sharing its start are
/// active; the first later line is next.
fn line_kinds(starts: &[u32], active: Option<usize>) -> Vec<LineKind> {
    let active_start = active.and_then(|index| starts.get(index).copied());
    let next = match active_start {
        Some(start) => starts.iter().position(|line| *line > start),
        None => (!starts.is_empty()).then_some(0),
    };
    starts
        .iter()
        .enumerate()
        .map(|(index, start)| match active_start {
            Some(active) if *start == active => LineKind::Active,
            _ if Some(index) == next => LineKind::Next,
            Some(active) if *start < active => LineKind::Past,
            _ => LineKind::Upcoming,
        })
        .collect()
}

/// Player-bar button that opens the lyrics panel.
#[component]
pub fn LyricsButton() -> Element {
    let state = use_context::<AppState>();
    let open = *state.lyrics.panel_open.read();
    let has_media = state.current.read().is_some();
    rsx! {
        button {
            class: if open { "player-btn player-btn-small player-btn-active" } else { "player-btn player-btn-small" },
            title: "Letra",
            aria_label: "Letra",
            aria_pressed: "{open}",
            disabled: !has_media,
            onclick: move |_| state.toggle_lyrics_panel(),
            Icon { name: "mic", size: 15 }
        }
    }
}

/// Side panel with the lyrics of what plays. It shares its place with the
/// queue and activity panels, so either of them opening closes it.
#[component]
pub fn LyricsPanel() -> Element {
    let state = use_context::<AppState>();
    let others = state.clone();
    use_effect(move || {
        if *others.queue_open.read() || *others.activity.open.read() {
            let mut panel = others.lyrics.panel_open;
            panel.set(false);
        }
    });
    let mut backdrop = state.lyrics.panel_open;
    let mut close = state.lyrics.panel_open;
    rsx! {
        div {
            class: "lyrics-backdrop",
            onclick: move |_| backdrop.set(false),
        }
        aside {
            class: "lyrics-panel",
            role: "dialog",
            aria_label: "Letra",
            div {
                class: "lyrics-panel__header",
                span {
                    class: "lyrics-panel__title",
                    "Letra"
                }
                LyricsTools {}
                button {
                    class: "btn-icon-sm",
                    title: "Cerrar (Esc)",
                    aria_label: "Cerrar la letra",
                    onclick: move |_| close.set(false),
                    Icon { name: "close", size: 15 }
                }
            }
            LyricsLines { stage: false }
        }
    }
}

/// Lyrics in the place of the cover of the full-screen view.
#[component]
pub fn LyricsStage() -> Element {
    rsx! {
        div {
            class: "lyrics-stage",
            div {
                class: "lyrics-stage__tools",
                LyricsTools {}
            }
            LyricsLines { stage: true }
        }
    }
}

/// Source of the shown lyrics and, when they are synced, the sync offset.
#[component]
fn LyricsTools() -> Element {
    let state = use_context::<AppState>();
    let view = state.lyrics.view.read().clone();
    let offset = *state.lyrics.offset_ms.read();
    let LyricsView::Found(shown) = view else {
        return rsx! {};
    };
    let later = state.clone();
    let earlier = state.clone();
    let reset = state.clone();
    rsx! {
        div {
            class: "lyrics-tools",
            span {
                class: "lyrics-tools__source",
                title: "Origen de la letra",
                "{source_label(shown.source)}"
            }
            if shown.lyrics.is_synced() {
                div {
                    class: "lyrics-tools__sync",
                    role: "group",
                    aria_label: "Sincronía de la letra",
                    button {
                        class: "lyrics-tools__step",
                        title: "Retrasar la letra 0,5 s",
                        aria_label: "Retrasar la letra 0,5 segundos",
                        onclick: move |_| later.nudge_lyrics(-OFFSET_STEP_MS),
                        "−"
                    }
                    button {
                        class: "lyrics-tools__offset",
                        title: "Restablecer la sincronía",
                        disabled: offset == 0,
                        onclick: move |_| reset.reset_lyrics_offset(),
                        "{offset_label(offset)}"
                    }
                    button {
                        class: "lyrics-tools__step",
                        title: "Adelantar la letra 0,5 s",
                        aria_label: "Adelantar la letra 0,5 segundos",
                        onclick: move |_| earlier.nudge_lyrics(OFFSET_STEP_MS),
                        "+"
                    }
                }
            }
        }
    }
}

/// The lyrics of what plays, or why there are none. Loads them whenever the
/// media changes while on screen and follows the shared playback poll.
#[component]
fn LyricsLines(stage: bool) -> Element {
    let state = use_context::<AppState>();
    let current = state.current;
    let current_id = use_memo(move || current.read().as_ref().map(|track| track.id.clone()));
    let loader = state.clone();
    use_effect(move || {
        current_id.read();
        loader.refresh_lyrics(false);
    });
    let clock = state.clone();
    use_effect(move || {
        clock.position.read();
        clock.rate.read();
        clock.playing.read();
        clock.buffering.read();
        clock.lyrics.offset_ms.read();
        clock.follow_lyrics_clock();
    });
    let view = state.lyrics.view;
    let position = state.position;
    let offset = state.lyrics.offset_ms;
    let active = use_memo(move || {
        let view = view.read();
        let LyricsView::Found(shown) = &*view else {
            return None;
        };
        active_line(
            &shown.lyrics,
            lyrics_clock_ms(*position.read(), *offset.read()),
        )
    });
    let mut smooth = use_signal(|| false);
    use_hook(move || {
        spawn(async move {
            if let Ok(value) = document::eval(REDUCED_MOTION_QUERY).await {
                smooth.set(value.as_bool() == Some(false));
            }
        })
    });
    let scrolled_at = use_hook(|| Rc::new(std::cell::Cell::new(None::<Instant>)));
    let mut container = use_signal(|| None::<Rc<MountedData>>);
    let running = state.lyrics_running();
    let sync = *state.lyrics.sync.read();
    let remote = state
        .current
        .read()
        .as_ref()
        .is_some_and(crate::remote::is_remote);
    let missing_hint = if remote {
        "El catálogo LRCLIB todavía no tiene esta canción."
    } else {
        "Si tienes su letra en un archivo .lrc, ponlo junto a la canción con el mismo nombre."
    };
    let shown = match view.read().clone() {
        LyricsView::Found(shown) => shown,
        LyricsView::Idle => {
            return rsx! {
                LyricsMessage { title: "Nada en reproducción", hint: "Reproduce una canción para ver su letra." }
            }
        }
        LyricsView::Loading => {
            return rsx! {
                LyricsMessage { title: "Buscando la letra…", hint: "", loading: true }
            }
        }
        LyricsView::Instrumental => {
            return rsx! {
                LyricsMessage { title: "Instrumental", hint: "Esta canción no tiene letra que cantar." }
            }
        }
        LyricsView::Missing { asked_online: true } => {
            return rsx! {
                LyricsMessage {
                    title: "No encontramos la letra",
                    hint: missing_hint,
                    retry: true,
                }
            }
        }
        LyricsView::Missing {
            asked_online: false,
        } => {
            return rsx! {
                LyricsMessage {
                    title: "Esta canción no trae letra",
                    hint: "Activa «Buscar letras en línea» en Ajustes → Letras para buscarla en LRCLIB.",
                }
            }
        }
        LyricsView::Unavailable { busy: true } => {
            return rsx! {
                LyricsMessage { title: "El catálogo de letras está ocupado", hint: "Vuelve a intentarlo en unos minutos." }
            }
        }
        LyricsView::Unavailable { busy: false } => {
            return rsx! {
                LyricsMessage {
                    title: "No se pudo conectar con el catálogo de letras",
                    hint: "Revisa tu conexión y vuelve a intentarlo.",
                    retry: true,
                }
            }
        }
    };
    let surface = if stage {
        "lyrics-lines lyrics-lines--stage"
    } else {
        "lyrics-lines"
    };
    if !shown.lyrics.is_synced() {
        return rsx! {
            div {
                class: "{surface} lyrics-lines--plain",
                p {
                    class: "lyrics-note",
                    "Letra sin sincronizar"
                }
                for (index, line) in shown.lyrics.lines().iter().enumerate() {
                    p {
                        key: "{index}",
                        class: if line.text.is_empty() { "lyric-plain lyric-plain--break" } else { "lyric-plain" },
                        "{line.text}"
                    }
                }
            }
        };
    }
    let starts: Vec<u32> = shown
        .lyrics
        .lines()
        .iter()
        .map(|line| line.start_ms)
        .collect();
    let kinds = line_kinds(&starts, active());
    let lyrics = SharedLyrics(shown);
    let wheel = scrolled_at.clone();
    let follow = Follow {
        scrolled_at,
        smooth: *smooth.read(),
        container,
    };
    rsx! {
        div {
            class: if running { "{surface}" } else { "{surface} lyrics-lines--paused" },
            onmounted: move |event| container.set(Some(event.data())),
            onwheel: move |_| wheel.set(Some(Instant::now())),
            div { class: "lyrics-lines__spacer", aria_hidden: "true" }
            for (index, kind) in kinds.into_iter().enumerate() {
                LyricRow {
                    key: "{index}",
                    lyrics: lyrics.clone(),
                    index,
                    kind,
                    sync,
                    follow: follow.clone(),
                }
            }
            div { class: "lyrics-lines__spacer", aria_hidden: "true" }
        }
    }
}

/// Whether and how the view follows the sung line.
#[derive(Clone)]
struct Follow {
    scrolled_at: Rc<std::cell::Cell<Option<Instant>>>,
    smooth: bool,
    /// The scrolling list; only it scrolls, never the view around it.
    container: Signal<Option<Rc<MountedData>>>,
}

impl PartialEq for Follow {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.scrolled_at, &other.scrolled_at)
            && self.smooth == other.smooth
            && self.container == other.container
    }
}

#[component]
fn LyricRow(
    lyrics: SharedLyrics,
    index: usize,
    kind: LineKind,
    sync: u64,
    follow: Follow,
) -> Element {
    let state = use_context::<AppState>();
    let Some(line) = lyrics.0.lyrics.lines().get(index) else {
        return rsx! {};
    };
    let text = line.text.clone();
    rsx! {
        button {
            class: kind.class(),
            tabindex: "-1",
            aria_current: if kind == LineKind::Active { "true" } else { "false" },
            onclick: move |_| state.seek_to_lyric(index),
            if kind == LineKind::Active {
                ScrollAnchor { follow }
            }
            if text.is_empty() {
                span {
                    class: "lyric-dots",
                    role: "img",
                    aria_label: "Instrumental",
                    i {}
                    i {}
                    i {}
                }
            } else if matches!(kind, LineKind::Active | LineKind::Next) {
                LyricFill { key: "{sync}", lyrics: lyrics.clone(), index }
            } else {
                "{text}"
            }
        }
    }
}

/// Mounted when its line becomes active: scrolls the list so the line sits at
/// `FOCUS_POINT`, unless the user scrolled a moment ago. Only the list
/// scrolls; `scrollIntoView` would also move the full-screen view around it.
#[component]
fn ScrollAnchor(follow: Follow) -> Element {
    rsx! {
        span {
            class: "lyric-line__anchor",
            aria_hidden: "true",
            onmounted: move |event| {
                let paused = follow
                    .scrolled_at
                    .get()
                    .is_some_and(|at| at.elapsed() < FOLLOW_PAUSE);
                if paused {
                    return;
                }
                let container = follow.container;
                let behavior = if follow.smooth { ScrollBehavior::Smooth } else { ScrollBehavior::Instant };
                let line = event.data();
                // Runs after every mount event of this render, so a list that
                // mounted together with its first active line is known.
                spawn(async move {
                    let Some(list) = container.peek().clone() else {
                        return;
                    };
                    let (Ok(line_box), Ok(list_box), Ok(offset)) = (
                        line.get_client_rect().await,
                        list.get_client_rect().await,
                        list.get_scroll_offset().await,
                    ) else {
                        return;
                    };
                    let top = offset.y + line_box.min_y() - list_box.min_y()
                        - (list_box.height() * FOCUS_POINT - line_box.height() / 2.0);
                    let _ = list
                        .scroll(PixelsVector2D::new(offset.x, top.max(0.0)), behavior)
                        .await;
                });
            },
        }
    }
}

/// The karaoke fill of one line. Its timing is computed once, when mounted,
/// because a running CSS animation must never see its timing change; a new
/// `sync` key mounts a fresh one.
#[component]
fn LyricFill(lyrics: SharedLyrics, index: usize) -> Element {
    let state = use_context::<AppState>();
    let spans = use_hook(|| {
        let now = state.lyrics_time_ms();
        let rate = f64::from(*state.rate.peek());
        Rc::new(
            line_segments(&lyrics.0.lyrics, index, &KaraokeConfig::default())
                .into_iter()
                .map(|segment| {
                    let (delay, length) = segment.schedule(now, rate);
                    (
                        segment.text,
                        delay.round() as i64,
                        length.round().max(1.0) as i64,
                    )
                })
                .collect::<Vec<_>>(),
        )
    });
    rsx! {
        for (position, (text, delay, length)) in spans.iter().enumerate() {
            span {
                key: "{position}",
                class: "lyric-fill",
                style: "animation-delay:{delay}ms;animation-duration:{length}ms",
                "{text}"
            }
        }
    }
}

#[component]
fn LyricsMessage(
    title: String,
    hint: String,
    #[props(default = false)] loading: bool,
    // A flag rather than an optional handler: these messages replace one
    // another in place, and Dioxus 0.7.10 frees a handler that appears later.
    #[props(default = false)] retry: bool,
) -> Element {
    let state = use_context::<AppState>();
    rsx! {
        div {
            class: "lyrics-message",
            role: "status",
            if loading {
                Icon { name: "loading", size: 18 }
            } else {
                Icon { name: "mic", size: 18 }
            }
            p {
                class: "lyrics-message__title",
                "{title}"
            }
            if !hint.is_empty() {
                p {
                    class: "lyrics-message__hint",
                    "{hint}"
                }
            }
            if retry {
                button {
                    class: "btn-secondary",
                    onclick: move |_| state.refresh_lyrics(true),
                    "Buscar de nuevo"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{remote_track, render, TestApp};
    use yt_core::Lyrics;

    fn panel() -> Element {
        rsx! { LyricsPanel {} }
    }

    fn show(state: &mut AppState, text: &str, source: LyricsSource) {
        let track = remote_track("aqz-KE-bpKQ", "Bailando");
        state.current.set(Some(track.clone()));
        state.lyrics.panel_open.set(true);
        state
            .lyrics
            .view
            .set(LyricsView::Found(Rc::new(ShownLyrics {
                media_id: track.id,
                lyrics: Lyrics::parse(text).unwrap(),
                source,
            })));
    }

    #[test]
    fn offsets_read_as_signed_seconds_with_a_decimal_comma() {
        assert_eq!(offset_label(0), "Sincronía");
        assert_eq!(offset_label(500), "+0,5 s");
        assert_eq!(offset_label(-1_500), "−1,5 s");
        assert_eq!(offset_label(30_000), "+30,0 s");
        assert_eq!(offset_label(1), "+0,1 s");
    }

    #[test]
    fn line_kinds_mark_the_sung_group_and_the_next_line() {
        use LineKind::*;
        assert_eq!(line_kinds(&[1, 2, 3], None), [Next, Upcoming, Upcoming]);
        assert_eq!(
            line_kinds(&[1, 2, 2, 3, 4], Some(1)),
            [Past, Active, Active, Next, Upcoming]
        );
        assert_eq!(line_kinds(&[1, 2], Some(1)), [Past, Active]);
        assert!(line_kinds(&[], None).is_empty());
    }

    #[test]
    fn the_panel_shows_synced_lines_with_their_source_and_sync_controls() {
        let html = render(panel, |state| {
            show(
                state,
                "[00:00.50]Yo te miro\n[00:03.00]Se me corta la respiración\n[00:06.00]",
                LyricsSource::Lrclib,
            );
            state.position.set(1.0);
        });
        for text in [
            "Letra",
            "LRCLIB",
            "Sincronía",
            "Adelantar la letra 0,5 s",
            "lyric-line--active",
            "lyric-fill",
            "animation-duration:",
            "Se me corta la respiración",
            "lyric-line--next",
            "lyric-dots",
            "lyrics-lines--paused",
        ] {
            assert!(html.contains(text), "missing {text} in {html}");
        }
    }

    #[test]
    fn plain_lyrics_and_every_empty_state_explain_themselves() {
        let plain = render(panel, |state| {
            show(state, "First line\n\nSecond line", LyricsSource::Embedded);
        });
        for text in [
            "Letra sin sincronizar",
            "First line",
            "Etiquetas del archivo",
        ] {
            assert!(plain.contains(text), "missing {text}");
        }
        assert!(!plain.contains("Sincronía"));
        for (view, text) in [
            (LyricsView::Idle, "Nada en reproducción"),
            (LyricsView::Loading, "Buscando la letra…"),
            (LyricsView::Instrumental, "Instrumental"),
            (
                LyricsView::Missing { asked_online: true },
                "Buscar de nuevo",
            ),
            (
                LyricsView::Missing {
                    asked_online: false,
                },
                "Buscar letras en línea",
            ),
            (LyricsView::Unavailable { busy: true }, "está ocupado"),
            (
                LyricsView::Unavailable { busy: false },
                "No se pudo conectar",
            ),
        ] {
            let html = render(panel, move |state| {
                state.lyrics.panel_open.set(true);
                state.lyrics.view.set(view.clone());
            });
            assert!(html.contains(text), "missing {text}");
        }
    }

    #[test]
    fn a_lookup_that_ends_in_a_retry_message_keeps_rendering() {
        let mut app = TestApp::showing(panel);
        app.step(|state| {
            state.lyrics.panel_open.set(true);
            state.lyrics.view.set(LyricsView::Loading);
        });
        app.step(|state| {
            state
                .lyrics
                .view
                .set(LyricsView::Missing { asked_online: true })
        });
        for playing in [true, false, true] {
            app.step(move |state| state.playing.set(playing));
        }
        assert!(dioxus_ssr::render(&app.dom).contains("Buscar de nuevo"));
    }
}
