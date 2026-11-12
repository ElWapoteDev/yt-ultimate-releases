//! Full-screen now-playing view. See `docs/systems/Playback.md`.

use crate::{
    app::{artwork_src, Thumb},
    favorites_ui::FavoriteButton,
    icons::Icon,
    player::{SeekBar, TransportControls},
    state::AppState,
};
use dioxus::{desktop::DesktopContext, prelude::*};
use yt_core::Track;

/// Upcoming songs listed beside the artwork.
const UP_NEXT_ROWS: usize = 4;

impl AppState {
    /// Open the full-screen view. A video opens its own stage full screen,
    /// where the video element owns the picture.
    pub(crate) fn open_now_playing(&self) {
        let Some(track) = self.current.peek().clone() else {
            return;
        };
        if track.media_type == "video" {
            let mut stage = self.video_open;
            stage.set(true);
            crate::video::command(self.clone(), serde_json::json!({"op": "fullscreen"}));
            return;
        }
        let mut open = self.now_playing_open;
        open.set(true);
    }

    pub(crate) fn close_now_playing(&self) {
        let mut open = self.now_playing_open;
        open.set(false);
    }
}

/// Enter window full screen while the view is mounted and restore the
/// previous window state when it closes.
fn use_window_fullscreen() {
    let window = try_consume_context::<DesktopContext>();
    let entered = use_hook(|| {
        window.as_ref().is_some_and(|window| {
            let already = window.window.fullscreen().is_some();
            if !already {
                window.set_fullscreen(true);
            }
            !already
        })
    });
    use_drop(move || {
        if let Some(window) = window.filter(|_| entered) {
            window.set_fullscreen(false);
        }
    });
}

#[component]
pub fn NowPlaying() -> Element {
    use_window_fullscreen();
    let state = use_context::<AppState>();
    let Some(track) = state.current.read().clone() else {
        return rsx! {};
    };
    let playing = *state.playing.read();
    let preferences = state.settings.read().playback.clone();
    let queue = state.queue.read().clone();
    let suggestions = state.autoplay.upcoming.read().clone();
    let from_autoplay = queue.is_empty();
    let up_next: Vec<Track> = if from_autoplay { suggestions } else { queue }
        .into_iter()
        .take(UP_NEXT_ROWS)
        .collect();
    let art = artwork_src(&track);
    // The blend's show, while two songs sound together.
    let blend = *state.blend.read();
    let show = blend.zip(state.blend_scene.read().clone());
    let mixing_as = *state.mixing_as.read();
    let lyrics_open = *state.lyrics.stage_open.read();
    let close = state.clone();
    let lyrics = state.clone();
    let dj = state.clone();
    let autoplay = state.clone();
    let mut queue_panel = state.clone();
    let artist_link = state.clone();
    let station = state.clone();
    let credits = state.clone();
    rsx! {
        style { {include_str!("now_playing_ui.css")} }
        div {
            class: if playing { "now-playing now-playing--playing" } else { "now-playing" },
            role: "dialog",
            aria_modal: "true",
            aria_label: "Reproduciendo ahora",
            div { class: "now-playing__backdrop", aria_hidden: "true",
                Thumb { src: art.clone(), size: "full", video: false }
            }
            div { class: "now-playing__aurora", aria_hidden: "true",
                span { class: "np-aurora__blob np-aurora__blob--1" }
                span { class: "np-aurora__blob np-aurora__blob--2" }
                span { class: "np-aurora__blob np-aurora__blob--3" }
                span { class: "np-aurora__blob np-aurora__blob--4" }
            }
            if let Some((blend, scene)) = show.clone() {
                crate::dj_ui::BlendStage {
                    blend,
                    scene,
                    playing,
                }
            }
            div { class: "now-playing__shade", aria_hidden: "true" }
            header { class: "now-playing__header",
                button {
                    class: "now-playing__icon-button",
                    title: "Cerrar (Esc)",
                    aria_label: "Cerrar pantalla completa",
                    onclick: move |_| close.close_now_playing(),
                    Icon { name: "chevron-down", size: 22 }
                }
                span { class: "now-playing__context",
                    if crate::remote::is_remote(&track) { "Reproduciendo desde YouTube" } else { "Reproduciendo desde tu biblioteca" }
                }
                span { class: "now-playing__spacer" }
            }
            div { class: "now-playing__body",
                if lyrics_open {
                    crate::lyrics_ui::LyricsStage {}
                } else {
                    div {
                        class: "now-playing__art-stack",
                        if let Some((blend, scene)) = show.clone() {
                            crate::dj_ui::BlendRings {
                                blend,
                                scene,
                                playing,
                            }
                        }
                        div { class: "now-playing__art",
                            Thumb { src: art, size: "full", video: false, icon_size: Some(64) }
                        }
                        if let Some((blend, scene)) = show.clone() {
                            crate::dj_ui::LeavingCover {
                                from: scene.from,
                                progress: blend.progress,
                                stage: true,
                            }
                        }
                    }
                }
                div { class: "now-playing__panel",
                    if let Some((blend, scene)) = show {
                        crate::dj_ui::BlendBanner {
                            blend,
                            name: mixing_as,
                            from: scene.from.title,
                            to: track.title.clone(),
                        }
                    }
                    div { class: "now-playing__titles",
                        div { style: "min-width:0",
                            h1 { class: "now-playing__title", title: "{track.title}", "{track.title}" }
                            if track.artist.is_empty() {
                                p { class: "now-playing__artist", "Artista desconocido" }
                            } else {
                                button {
                                    class: "now-playing__artist now-playing__artist--link",
                                    title: "Ver artista",
                                    onclick: {
                                        let state = artist_link.clone();
                                        let song = track.clone();
                                        move |_| {
                                            state.close_now_playing();
                                            state.open_track_artist(&song);
                                        }
                                    },
                                    "{track.artist}"
                                }
                            }
                        }
                        FavoriteButton { track: track.clone(), size: 22, class: "now-playing__favorite" }
                    }
                    div { class: "now-playing__seek", SeekBar {} }
                    TransportControls { large: true }
                    div {
                        class: "now-playing__actions",
                        role: "toolbar",
                        aria_label: "Más opciones",
                        NowPlayingAction {
                            icon: "quote",
                            label: "Letra",
                            detail: "Mostrar la letra en lugar de la carátula",
                            pressed: Some(lyrics_open),
                            onclick: move |_| lyrics.set_lyrics_stage(!lyrics_open),
                        }
                        NowPlayingAction {
                            icon: "disc",
                            label: "Modo DJ",
                            detail: if preferences.dj_mode {
                                format!("Fundido de {} s", preferences.crossfade_seconds)
                            } else {
                                "Fundir canciones al cambiar".to_owned()
                            },
                            pressed: Some(preferences.dj_mode),
                            onclick: move |_| dj.set_dj_mode(!preferences.dj_mode),
                        }
                        NowPlayingAction {
                            icon: "infinity",
                            label: "Reproducción automática",
                            detail: "Seguir con música parecida al terminar la cola",
                            pressed: Some(preferences.autoplay),
                            onclick: move |_| autoplay.set_autoplay(!preferences.autoplay),
                        }
                        NowPlayingAction {
                            icon: "radio",
                            label: "Crear estación",
                            detail: "Música sin fin a partir de esta canción",
                            onclick: {
                                let state = station.clone();
                                let seed = track.clone();
                                move |_| state.start_song_station(&seed)
                            },
                        }
                        NowPlayingAction {
                            icon: "users",
                            label: "Créditos",
                            detail: "Quién la escribió, la produjo y la publicó",
                            onclick: {
                                let state = credits.clone();
                                let song = track.clone();
                                move |_| state.show_credits(&song)
                            },
                        }
                    }
                    if !up_next.is_empty() {
                        div { class: "now-playing__next",
                            div { class: "now-playing__next-header",
                                span { if from_autoplay { "A continuación · automático" } else { "A continuación" } }
                                button {
                                    class: "np-link",
                                    onclick: move |_| {
                                        queue_panel.close_now_playing();
                                        queue_panel.queue_open.set(true);
                                    },
                                    "Ver cola"
                                }
                            }
                            for (index, item) in up_next.into_iter().enumerate() {
                                div { key: "{index}-{item.id}", class: "now-playing__next-row",
                                    Thumb { src: artwork_src(&item), size: "xs", icon_size: Some(12) }
                                    div { class: "now-playing__next-meta",
                                        span { class: "now-playing__next-title", "{item.title}" }
                                        span { class: "now-playing__next-artist", "{item.artist}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One quiet icon action of the full-screen view, in the language of shuffle
/// and repeat. `label` names it for assistive technology and starts its
/// tooltip, `detail` completes the tooltip, and `pressed` makes it a toggle
/// whose state shows as the accent color and a dot.
#[component]
fn NowPlayingAction(
    icon: &'static str,
    label: String,
    #[props(default)] detail: String,
    #[props(default = None)] pressed: Option<bool>,
    onclick: EventHandler<MouseEvent>,
) -> Element {
    let title = if detail.is_empty() {
        label.clone()
    } else {
        format!("{label} · {detail}")
    };
    rsx! {
        button {
            class: if pressed == Some(true) { "np-action np-action--on" } else { "np-action" },
            title,
            aria_label: label,
            aria_pressed: pressed.map(|pressed| pressed.to_string()),
            onclick: move |event| onclick.call(event),
            Icon { name: icon, size: 19 }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{remote_track, render};

    fn view() -> Element {
        rsx! { NowPlaying {} }
    }

    #[test]
    fn full_screen_view_shows_the_song_its_controls_and_what_follows() {
        let html = render(view, |state| {
            state
                .current
                .set(Some(remote_track("aqz-KE-bpKQ", "Bailando")));
            state
                .queue
                .set(vec![remote_track("abcdefghijk", "La Bicicleta")]);
            state.set_dj_mode(true);
        });
        for text in [
            "Bailando",
            "Reproduciendo desde YouTube",
            "Modo DJ · Fundido de 6 s",
            "Reproducción automática",
            "A continuación",
            "La Bicicleta",
            "Cerrar pantalla completa",
            "Posición de reproducción",
        ] {
            assert!(html.contains(text), "missing {text}");
        }
        assert!(render(view, |_| {}).is_empty());
    }

    #[test]
    fn a_blend_shows_both_songs_and_its_show_around_the_cover() {
        let blending = |state: &mut AppState| {
            state
                .current
                .set(Some(remote_track("abcdefghijk", "La que entra")));
            state.blend.set(Some(yt_media::Blend {
                mix: true,
                progress: 0.5,
            }));
            state.blend_scene.set(Some(crate::dj::BlendScene {
                from: remote_track("aqz-KE-bpKQ", "La que sale"),
                seconds: 16.0,
                beat_seconds: Some(0.5),
            }));
            state.mixing_as.set(Some(crate::dj::MixName {
                label: "Mashup",
                ai: true,
            }));
        };
        let html = render(view, blending);
        assert!(
            html.contains("aria-label=\"Mashup: de La que sale a La que entra\""),
            "{html}"
        );
        for part in [
            "dj-stage__rays",
            "dj-rings__ring",
            "dj-leaving--stage",
            "--dj-beat:0.500s",
        ] {
            assert!(html.contains(part), "missing {part}");
        }
        // The leaving song's cover lies over the playing one's.
        assert!(html.contains("/artwork/youtube:aqz-KE-bpKQ"), "{html}");
        let calm = render(view, |state| {
            state
                .current
                .set(Some(remote_track("abcdefghijk", "La que entra")));
        });
        for part in ["dj-banner", "dj-show", "dj-leaving"] {
            assert!(!calm.contains(part), "{part} without a blend");
        }
    }

    /// The opening tag of the button named `label`.
    fn button<'a>(html: &'a str, label: &str) -> &'a str {
        let name = html
            .find(&format!("aria-label=\"{label}\""))
            .unwrap_or_else(|| panic!("no button named {label}"));
        let start = html[..name].rfind("<button").unwrap();
        let end = name + html[name..].find('>').unwrap();
        &html[start..=end]
    }

    #[test]
    fn secondary_actions_are_bare_icons_named_by_their_tooltips() {
        let html = render(view, |state| {
            state
                .current
                .set(Some(remote_track("abcdefghijk", "Bailando")));
            state.set_dj_mode(true);
        });
        assert!(!html.contains("np-chip"));
        assert_eq!(html.matches("class=\"np-action").count(), 5);
        for (label, on) in [
            ("Letra", false),
            ("Modo DJ", true),
            ("Reproducción automática", true),
        ] {
            let tag = button(&html, label);
            assert!(tag.contains(&format!("aria-pressed=\"{on}\"")), "{tag}");
            assert_eq!(tag.contains("np-action--on"), on, "{tag}");
        }
        let station = button(&html, "Crear estación");
        assert!(!station.contains("aria-pressed"), "{station}");
        assert!(station.contains("title=\"Crear estación · Música sin fin"));
        let credits = button(&html, "Créditos");
        assert!(
            credits.contains("title=\"Créditos · Quién la escribió"),
            "{credits}"
        );
    }

    #[test]
    fn the_background_is_an_aurora_over_the_full_size_album_cover() {
        let html = render(view, |state| {
            state
                .current
                .set(Some(remote_track("abcdefghijk", "Bailando")));
            state.album_art.remember(yt_core::AlbumArtwork {
                video_id: "abcdefghijk".into(),
                url: Some("https://yt3.googleusercontent.com/albumCover".into()),
                checked_at: crate::discovery::unix_now(),
            });
        });
        assert_eq!(html.matches("np-aurora__blob np-aurora__blob--").count(), 4);
        // Backdrop and artwork both load the original cover.
        assert_eq!(
            html.matches("src=\"https://yt3.googleusercontent.com/albumCover=s0\"")
                .count(),
            2
        );
    }
}
