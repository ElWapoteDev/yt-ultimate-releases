use crate::{app::Thumb, icons::Icon, state::*};
use dioxus::prelude::*;
use std::time::{Duration, Instant};

#[component]
pub fn PlayerBar() -> Element {
    let s = use_context::<AppState>();
    let current = s.current.read().clone();
    let has_media = current.is_some();
    let video = current
        .as_ref()
        .is_some_and(|track| track.media_type == "video");
    let buffering = *s.stream_loading.read() || *s.buffering.read();
    let recovering = *s.recovering.read();
    let error = s.playback_error.read().clone();
    let volume = *s.volume.read();
    let rate = *s.rate.read();
    let dj = s.settings.read().playback.dj_mode;
    let blend = *s.blend.read();
    let mixing_as = *s.mixing_as.read();
    let next_mix = *s.next_mix.read();
    // The blend's show: the song that leaves and the timing of the music.
    let show = blend.zip(s.blend_scene.read().clone());
    let playing = *s.playing.read();
    let context = s.clone();
    let mute = s.clone();
    let change_volume = s.clone();
    let retry = s.clone();
    let dj_state = s.clone();
    let fullscreen = s.clone();
    let mut open_video = s.clone();
    let mut options = s.clone();
    let mut queue = s.clone();
    let mut close_options = s.clone();
    rsx! {
        footer {
            class: "player-bar",
            aria_label: "Reproductor",
            oncontextmenu: move |event| crate::context_menu::show(event, player_items(context.clone())),
            if let Some((blend, scene)) = show.clone() {
                crate::dj_ui::BlendBeam {
                    blend,
                    scene,
                    playing,
                }
            }
            div { class: "player-now",
                div {
                    class: "player-deck",
                    if let Some((blend, scene)) = show {
                        crate::dj_ui::BlendHalo {
                            blend,
                            scene: scene.clone(),
                            playing,
                        }
                        crate::dj_ui::LeavingCover {
                            from: scene.from,
                            progress: blend.progress,
                            stage: false,
                        }
                    }
                    div { class: if has_media { "player-artwork" } else { "player-artwork player-artwork--empty" },
                        if let Some(track) = current.clone() {
                            Thumb { src: crate::app::artwork_src(&track), size: "md", video: track.media_type == "video", icon_size: Some(18) }
                        } else {
                            Icon { name: "music", size: 18 }
                        }
                    }
                }
                div { class: "player-info",
                    if let Some(track) = current.clone() {
                        div { class: "player-song",
                            if crate::remote::is_remote(&track) {
                                Icon { name: "radio", size: 12 }
                            }
                            span { style: "overflow:hidden;text-overflow:ellipsis", "{track.title}" }
                            if buffering {
                                span { aria_label: "Cargando", Icon { name: "loading", size: 12 } }
                            }
                            if let Some(blend) = blend {
                                crate::dj_ui::BlendBadge {
                                    blend,
                                    name: mixing_as,
                                }
                            } else if let Some(next) = next_mix {
                                crate::dj_ui::NextMixChip {
                                    next,
                                }
                            }
                        }
                        div {
                            class: "player-artist",
                            role: if error.is_some() { "alert" } else { "status" },
                            title: error.clone().unwrap_or_default(),
                            if let Some(error) = &error {
                                "{error}"
                            } else if recovering {
                                "Reconectando stream…"
                            } else if !track.artist.is_empty() {
                                button {
                                    class: "player-artist__link",
                                    title: "Ver artista",
                                    onclick: {
                                        let state = s.clone();
                                        let song = track.clone();
                                        move |_| state.open_track_artist(&song)
                                    },
                                    "{track.artist}"
                                }
                            } else if video {
                                "Vídeo"
                            } else {
                                "Audio"
                            }
                        }
                    } else {
                        div { class: "player-song player-song--empty", "Nada en reproducción" }
                    }
                }
                if let Some(track) = current {
                    crate::favorites_ui::FavoriteButton { track, size: 15 }
                }
            }
            div { class: "player-center",
                TransportControls {}
                SeekBar {}
            }
            div { class: "player-side",
                if error.is_some() {
                    button {
                        class: "player-btn player-btn-small",
                        title: "Reintentar reproducción",
                        aria_label: "Reintentar reproducción",
                        disabled: recovering,
                        onclick: move |_| retry.retry_playback(),
                        Icon { name: "retry", size: 15 }
                    }
                }
                div { class: "player-volume",
                    button {
                        class: "player-btn player-btn-small",
                        title: if volume == 0.0 { "Activar sonido" } else { "Silenciar" },
                        onclick: move |_| mute.toggle_mute(),
                        Icon { name: if volume == 0.0 { "volume-off" } else if volume < 0.55 { "volume-low" } else { "volume" }, size: 15 }
                    }
                    input {
                        class: "slider-volume",
                        r#type: "range",
                        aria_label: "Volumen",
                        min: "0",
                        max: "1",
                        step: "0.01",
                        value: "{volume}",
                        style: "--vol:{volume*100.0}%",
                        oninput: move |event| if let Ok(value) = event.value().parse() { change_volume.set_volume(value); },
                    }
                }
                button {
                    class: if dj { "player-btn player-btn-small player-btn-active" } else { "player-btn player-btn-small" },
                    title: if dj { "Modo DJ activado: fundidos entre canciones" } else { "Modo DJ: fundir canciones" },
                    aria_pressed: "{dj}",
                    onclick: move |_| dj_state.set_dj_mode(!dj),
                    Icon { name: "disc", size: 15 }
                }
                if video {
                    button {
                        class: "player-btn player-btn-small",
                        title: "Mostrar vídeo",
                        onclick: move |_| open_video.video_open.set(true),
                        Icon { name: "video", size: 15 }
                    }
                }
                button {
                    class: "player-btn player-btn-small",
                    title: "Pantalla completa",
                    aria_label: "Pantalla completa",
                    disabled: !has_media,
                    onclick: move |_| fullscreen.open_now_playing(),
                    Icon { name: "maximize", size: 14 }
                }
                div { class: "player-options",
                    button {
                        class: if *s.options_open.read() { "player-btn player-btn-small player-btn-active" } else { "player-btn player-btn-small" },
                        title: "Más controles",
                        aria_expanded: s.options_open.read().to_string(),
                        onclick: move |_| options.options_open.toggle(),
                        Icon { name: "more", size: 16 }
                    }
                    if *s.options_open.read() {
                        div { style: "position:fixed;inset:0;z-index:149", onclick: move |_| close_options.options_open.set(false) }
                        div { class: "player-options__popover", style: "z-index:150",
                            div { class: "player-options__label", "Velocidad" }
                            div { class: "player-options__rates",
                                for speed in [0.75f32, 1.0, 1.25, 1.5, 2.0] {
                                    button {
                                        class: if (rate - speed).abs() < 0.01 { "active" } else { "" },
                                        onclick: { let state = s.clone(); move |_| state.set_rate(speed) },
                                        "{speed}×"
                                    }
                                }
                            }
                            div { class: "player-options__divider" }
                            button {
                                onclick: { let state = s.clone(); move |_| { let position = *state.position.peek(); state.seek(position - 10.0); } },
                                Icon { name: "retry", size: 14 }
                                "Retroceder 10 s"
                            }
                            if video {
                                button {
                                    onclick: { let state = s.clone(); move |_| crate::video::command(state.clone(), serde_json::json!({"op":"pip"})) },
                                    Icon { name: "pip", size: 14 }
                                    "Picture in Picture"
                                }
                            }
                            div { class: "player-options__divider" }
                            div { class: "player-options__label", Icon { name: "timer", size: 13 } "Temporizador" }
                            div { class: "player-options__rates",
                                for minutes in [0u64, 15, 30, 60] {
                                    button {
                                        onclick: { let mut state = s.clone(); move |_| state.sleep_at.set(if minutes == 0 { None } else { Some(Instant::now() + Duration::from_secs(minutes * 60)) }) },
                                        if minutes == 0 { "No" } else { "{minutes} min" }
                                    }
                                }
                            }
                        }
                    }
                }
                crate::lyrics_ui::LyricsButton {}
                crate::activity_ui::ActivityButton {}
                button {
                    class: if *s.queue_open.read() { "player-btn player-btn-small queue-button player-btn-active" } else { "player-btn player-btn-small queue-button" },
                    title: "Cola de reproducción",
                    aria_expanded: s.queue_open.read().to_string(),
                    onclick: move |_| {
                        queue.queue_open.toggle();
                        queue.activity.open.set(false);
                    },
                    Icon { name: "playlist", size: 15 }
                    if !s.queue.read().is_empty() && !*s.queue_open.read() {
                        span { class: "queue-dot" }
                    }
                }
            }
        }
    }
}

/// Shuffle, previous, play, next and repeat; `large` is the full-screen size.
#[component]
pub fn TransportControls(#[props(default = false)] large: bool) -> Element {
    let s = use_context::<AppState>();
    let has_media = s.current.read().is_some();
    let is_playing = *s.playing.read();
    let buffering = *s.stream_loading.read() || *s.buffering.read();
    let shuffle_on = *s.shuffle.read();
    let repeat = *s.repeat.read();
    let can_skip = !s.queue.read().is_empty() || !s.autoplay.upcoming.read().is_empty();
    let (small, step, main) = if large { (20, 26, 26) } else { (14, 17, 16) };
    let play = s.clone();
    let previous = s.clone();
    let next = s.clone();
    let shuffle = s.clone();
    let cycle = s.clone();
    rsx! {
        div { class: if large { "player-controls np-controls" } else { "player-controls" },
            button {
                class: if shuffle_on { "player-btn player-btn-small player-btn-active" } else { "player-btn player-btn-small" },
                title: if shuffle_on { "Aleatorio activado" } else { "Aleatorio" },
                aria_pressed: "{shuffle_on}",
                onclick: move |_| shuffle.toggle_shuffle(),
                Icon { name: "shuffle", size: small }
            }
            button {
                class: "player-btn",
                title: "Anterior",
                disabled: !has_media,
                onclick: move |_| previous.previous(),
                Icon { name: "previous", size: step, fill: "currentColor" }
            }
            button {
                class: "player-btn player-btn-play",
                title: if is_playing { "Pausar" } else { "Reproducir" },
                disabled: !has_media,
                onclick: move |_| play.toggle_play(),
                Icon {
                    name: if buffering { "loading" } else if is_playing { "pause" } else { "play" },
                    size: main,
                    fill: if buffering { "none" } else { "currentColor" },
                    style: if !buffering && !is_playing { "margin-left:2px" } else { "" },
                }
            }
            button {
                class: "player-btn",
                title: "Siguiente",
                disabled: !can_skip,
                onclick: move |_| next.next(false),
                Icon { name: "next", size: step, fill: "currentColor" }
            }
            button {
                class: if repeat > 0 { "player-btn player-btn-small player-btn-active" } else { "player-btn player-btn-small" },
                title: match repeat { 1 => "Repetir todo", 2 => "Repetir uno", _ => "Repetir" },
                onclick: move |_| cycle.cycle_repeat(),
                Icon { name: if repeat == 2 { "repeat-one" } else { "repeat" }, size: small }
            }
        }
    }
}

/// Position, duration and a seekable track. While dragging, the shown time
/// follows the pointer and the seek happens on release; the value is parsed
/// and clamped in Rust, never trusted from the range element.
#[component]
pub fn SeekBar() -> Element {
    let s = use_context::<AppState>();
    let has_media = s.current.read().is_some();
    let duration = *s.duration.read();
    let position = *s.position.read();
    let mut scrub = use_signal(|| None::<f64>);
    let shown = scrub.read().unwrap_or(position);
    let mixing = s.blend.read().is_some();
    let zone = s
        .settings
        .read()
        .playback
        .dj_show_mix
        .then(|| *s.next_mix.read())
        .flatten()
        .and_then(|next| Some((crate::dj_ui::mix_zone(next, duration)?, next.preparing)));
    let percent = if duration > 0.0 {
        shown / duration * 100.0
    } else {
        0.0
    };
    let seek = s.clone();
    let key_seek = s.clone();
    rsx! {
        div { class: "player-progress-section",
            span { class: "player-time", if has_media { "{time_label(shown)}" } else { "--:--" } }
            div {
                class: "player-progress-track",
                class: if scrub.read().is_some() { "scrubbing" },
                class: if mixing { "mixing" },
                if let Some(((left, width), preparing)) = zone {
                    crate::dj_ui::MixZone {
                        left,
                        width,
                        preparing,
                    }
                }
                div {
                    class: "player-progress-fill",
                    style: "width:{percent.clamp(0.0, 100.0)}%",
                    if mixing {
                        crate::dj_ui::BlendSweep {}
                    }
                }
                input {
                    r#type: "range",
                    class: "rust-progress-hit",
                    aria_label: "Posición de reproducción",
                    aria_valuetext: "{time_label(shown)} de {time_label(duration)}",
                    min: "0",
                    max: "{duration.max(1.0)}",
                    step: "0.1",
                    value: "{shown}",
                    disabled: !has_media || duration <= 0.0,
                    oninput: move |event| if let Ok(value) = event.value().parse::<f64>() { scrub.set(Some(value)); },
                    onchange: move |event| {
                        if let Ok(value) = event.value().parse::<f64>() {
                            seek.seek(value);
                        }
                        scrub.set(None);
                    },
                    onpointercancel: move |_| scrub.set(None),
                    onkeydown: move |event| {
                        let current = *key_seek.position.peek();
                        let step = if event.modifiers().shift() { 30.0 } else { 5.0 };
                        let target = match event.key() {
                            Key::ArrowLeft => Some(current - step),
                            Key::ArrowRight => Some(current + step),
                            Key::Home => Some(0.0),
                            Key::End => Some(*key_seek.duration.peek()),
                            _ => None,
                        };
                        if let Some(target) = target {
                            event.prevent_default();
                            key_seek.seek(target);
                            scrub.set(None);
                        }
                    },
                }
            }
            span { class: "player-time", if has_media { "{time_label(duration)}" } else { "--:--" } }
        }
    }
}
fn player_items(state: AppState) -> Vec<crate::context_menu::Entry> {
    use crate::context_menu::Entry;
    let current = state.current.peek().clone();
    let play = state.clone();
    let back = state.clone();
    let forward = state.clone();
    let mut items = vec![
        Entry::action(
            if *state.playing.peek() {
                "Pausar"
            } else {
                "Reproducir"
            },
            move || play.toggle_play(),
        )
        .enabled(current.is_some()),
        Entry::action("Retroceder 10 segundos", move || {
            let position = *back.position.peek();
            back.seek(position - 10.0);
        })
        .enabled(current.is_some()),
        Entry::action("Adelantar 10 segundos", move || {
            let position = *forward.position.peek();
            forward.seek(position + 10.0);
        })
        .enabled(current.is_some()),
        Entry::separator(),
    ];
    if let Some(track) = current
        .clone()
        .filter(|track| !crate::remote::is_remote(track))
    {
        let chipmunk = state.clone();
        items.push(Entry::action(
            "Versión Alvin y las ardillas…",
            move || {
                let mut signal = chipmunk.chipmunk_track;
                signal.set(Some(track.clone()));
            },
        ));
        items.push(Entry::separator());
    }
    let path = current.map(|track| track.path).unwrap_or_default();
    let local = path.is_file();
    items.push(
        Entry::action(
            if cfg!(target_os = "macos") {
                "Mostrar en Finder"
            } else {
                "Mostrar en el Explorador"
            },
            move || crate::native::reveal_file(&state, &path),
        )
        .enabled(local),
    );
    items
}

#[component]
pub fn VideoPanel() -> Element {
    let mut s = use_context::<AppState>();
    let Some(track) = s.video_track.read().clone() else {
        return rsx! {};
    };
    let ended = s.clone();
    let error = s.clone();
    let mut native_play = s.clone();
    let mut native_pause = s.clone();
    let initialize = s.clone();
    let pip = s.clone();
    let polling = s.clone();
    use_hook(move || {
        spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(250)).await;
                if let Some(key) = crate::video::key(&polling) {
                    if let Ok(Some(snapshot)) =
                        crate::video::invoke(key.clone(), serde_json::json!({"op":"snapshot"}))
                            .await
                    {
                        if crate::video::key(&polling).as_ref() == Some(&key) {
                            crate::video::apply_snapshot(&polling, snapshot);
                        }
                    }
                }
            }
        })
    });
    let subtitles = crate::local_media::subtitle_tracks(&s.services.core, &track.id);
    let generation = *s.video_generation.read();
    let media_key = format!("{}:{generation}", track.id);
    let video_open = *s.video_open.read();
    let loaded_key = media_key.clone();
    let play_key = media_key.clone();
    let pause_key = media_key.clone();
    let ended_key = media_key.clone();
    let error_key = media_key.clone();
    let source = s
        .video_url
        .read()
        .clone()
        .unwrap_or_else(|| crate::local_media::media_url(&track.id));
    rsx! { div { class: if video_open { "video-stage video-stage--open" } else { "video-stage" }, role:"dialog",aria_hidden:(!video_open).to_string(),aria_label: "Reproductor de vídeo",
            div { class: "video-stage__chrome", span { class: "video-stage__title", "{track.title}" }
                div { class: "video-stage__actions",
                    button { title: "Imagen en imagen", onclick: move |_| crate::video::command(pip.clone(), serde_json::json!({"op":"pip"})), "Picture in Picture" }
                    button { title: "Cerrar vista de vídeo", onclick: move |_| { s.video_open.set(false); s.persist_playback(); }, "Cerrar" }
                }
            }
            video { key: "{media_key}", id: "ytu-video", "data-media-key": "{media_key}", class: "video-stage__media", src: source, controls: true, playsinline: true,
                onloadedmetadata: move |_| if crate::video::key(&initialize).as_ref()==Some(&loaded_key){crate::video::command(initialize.clone(), serde_json::json!({"op":"init","position":*initialize.position.peek(),"volume":*initialize.volume.peek(),"rate":*initialize.rate.peek(),"play":*initialize.playing.peek()}));},
                onplay: move |_| if crate::video::key(&native_play).as_ref()==Some(&play_key){native_play.playing.set(true);},
                onpause: move |_| if crate::video::key(&native_pause).as_ref()==Some(&pause_key){ native_pause.playing.set(false); native_pause.persist_playback(); },
                onended: move |_| if crate::video::key(&ended).as_ref()==Some(&ended_key){ended.next(true);},
                onerror: move |_| if crate::video::key(&error).as_ref()==Some(&error_key){error.recover_remote("El sistema no puede reproducir este formato de vídeo.".into());},
                for subtitle in subtitles { track { kind: "subtitles", src: subtitle.src, srclang: subtitle.language, label: subtitle.label, default: subtitle.default } }
            }
    } }
}

#[component]
pub fn QueuePanel() -> Element {
    rsx! {crate::queue_ui::QueuePanel{}}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{remote_track, render};

    fn bar() -> Element {
        rsx! { PlayerBar {} }
    }

    #[test]
    fn a_blend_lights_the_bar_and_slides_the_leaving_cover_away() {
        let html = render(bar, |state| {
            state
                .current
                .set(Some(remote_track("abcdefghijk", "La que entra")));
            state.playing.set(true);
            state.blend.set(Some(yt_media::Blend {
                mix: true,
                progress: 0.5,
            }));
            state.blend_scene.set(Some(crate::dj::BlendScene {
                from: remote_track("aqz-KE-bpKQ", "La que sale"),
                seconds: 16.0,
                beat_seconds: Some(0.5),
            }));
        });
        for part in [
            "dj-show dj-beam dj-show--beat",
            "dj-show dj-halo dj-show--beat",
            "dj-leaving--bar",
            "/artwork/youtube:aqz-KE-bpKQ",
            "Mezclando",
            "dj-sweep",
        ] {
            assert!(html.contains(part), "missing {part}: {html}");
        }
        let calm = render(bar, |state| {
            state
                .current
                .set(Some(remote_track("abcdefghijk", "La que entra")));
        });
        assert!(
            !calm.contains("dj-show") && !calm.contains("dj-leaving"),
            "no show without a blend"
        );
    }
}
