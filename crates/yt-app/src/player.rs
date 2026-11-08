use crate::{
    app::{Equalizer, Thumb},
    icons::Icon,
    state::*,
};
use dioxus::prelude::*;
use std::time::{Duration, Instant};
use yt_core::Track;

#[component]
pub fn PlayerBar() -> Element {
    let s = use_context::<AppState>();
    let current = s.current.read().clone();
    let has_media = current.is_some();
    let is_video = current
        .as_ref()
        .is_some_and(|track| track.media_type == "video");
    let audio_controls = has_media && !is_video;
    let is_playing = *s.playing.read();
    let duration = *s.duration.read();
    let position = *s.position.read();
    let volume = *s.volume.read();
    let rate = *s.rate.read();
    let shuffle_state = s.clone();
    let repeat_state = s.clone();
    let previous_state = s.clone();
    let next_state = s.clone();
    let play_state = s.clone();
    let seek_state = s.clone();
    let volume_state = s.clone();
    let mute_state = s.clone();
    let mut queue_state = s.clone();
    let mut options_state = s.clone();
    let back_state = s.clone();
    let forward_state = s.clone();
    rsx! { footer { class: "player-bar", aria_label: "Reproductor",
        div { class: "player-now",
            if let Some(track) = current {
                Thumb { src: crate::app::artwork_src(&track), size: "md" }
                div { class: "player-info", div { class: "player-song", "{track.title}" } div { class: "player-artist", "{track.artist}" } }
            } else {
                div { class: "player-artwork player-artwork--empty", Icon { name: "music", size: 20 } }
                div { class: "player-info", div { class: "player-song player-song--empty", "Sin reproducción" } div { class: "player-artist", "Elige algo para escuchar" } }
            }
        }
        div { class: "player-center",
            div { class: "player-controls",
                button { class: if *s.shuffle.read() { "player-btn player-btn-small player-btn-active" } else { "player-btn player-btn-small" }, title: "Aleatorio", aria_pressed: *s.shuffle.read(), onclick: move |_| shuffle_state.toggle_shuffle(), Icon { name: "shuffle", size: 14 } }
                button { class: "player-btn", title: "Anterior", disabled: !has_media, onclick: move |_| previous_state.previous(), Icon { name: "previous", size: 17 } }
                button { class: "player-btn player-btn-play", title: if is_video { "Mostrar vídeo" } else if is_playing { "Pausar" } else { "Reproducir" }, disabled: !has_media, onclick: move |_| play_state.toggle_play(), Icon { name: if is_video { "video" } else if is_playing { "pause" } else { "play" }, size: 16 } }
                button { class: "player-btn", title: "Siguiente", disabled: s.queue.read().is_empty(), onclick: move |_| next_state.next(false), Icon { name: "next", size: 17 } }
                button { class: if *s.repeat.read() > 0 { "player-btn player-btn-small player-btn-active" } else { "player-btn player-btn-small" }, title: match *s.repeat.read() { 1 => "Repetir todo", 2 => "Repetir uno", _ => "Repetir" }, onclick: move |_| repeat_state.cycle_repeat(),
                    if *s.repeat.read() == 2 { span { style: "font-size:11px;font-weight:700", "1" } } else { Icon { name: "repeat", size: 14 } }
                }
            }
            div { class: "player-progress-section",
                span { class: "player-time", if has_media { "{time_label(position)}" } else { "--:--" } }
                input { r#type: "range", class: "rust-progress", aria_label: "Posición de reproducción", min: "0", max: "{duration.max(1.0)}", step: "0.1", value: "{position}", disabled: !audio_controls, oninput: move |e| { if let Ok(value) = e.value().parse::<f64>() { seek_state.seek(value); } } }
                span { class: "player-time", if has_media { "{time_label(duration)}" } else { "--:--" } }
            }
        }
        div { class: "player-side",
            div { class: "player-volume",
                button { class: "player-btn player-btn-small", disabled: is_video, title: if volume == 0.0 { "Activar sonido" } else { "Silenciar" }, onclick: move |_| { let value = if *mute_state.volume.read() == 0.0 { 0.8 } else { 0.0 }; mute_state.set_volume(value); }, Icon { name: "volume", size: 15 } }
                input { class: "slider-volume", r#type: "range", disabled: is_video, aria_label: "Volumen", min: "0", max: "1", step: "0.01", value: "{volume}", style: "--vol:{volume * 100.0}%", oninput: move |e| { if let Ok(value) = e.value().parse() { volume_state.set_volume(value); } } }
            }
            div { class: "player-options",
                button { class: "player-btn player-btn-small", title: "Más controles", aria_expanded: *s.options_open.read(), onclick: move |_| options_state.options_open.toggle(), Icon { name: "more", size: 16 } }
                if *s.options_open.read() {
                    div { class: "player-options__popover",
                        div { class: "player-options__label", "Velocidad" }
                        div { class: "player-options__rates", for speed in [0.75f32, 1.0, 1.25, 1.5, 2.0] {
                            button { disabled: is_video, class: if (rate - speed).abs() < 0.01 { "active" } else { "" }, onclick: { let state = s.clone(); move |_| state.set_rate(speed) }, "{speed}×" }
                        } }
                        div { class: "player-options__divider" }
                        if is_video { p { class: "rust-settings-note", "Usa los controles de la ventana de vídeo." } }
                        button { disabled: !audio_controls, onclick: move |_| { let position = *back_state.position.read(); back_state.seek((position - 10.0).max(0.0)); }, "Retroceder 10 s" }
                        button { disabled: !audio_controls, onclick: move |_| { let position = *forward_state.position.read(); forward_state.seek(position + 10.0); }, "Avanzar 10 s" }
                        div { class: "player-options__divider" }
                        div { class: "player-options__label", "Temporizador" }
                        div { class: "player-options__rates", for minutes in [0u64, 15, 30, 60] {
                            button { onclick: { let mut state = s.clone(); move |_| { state.sleep_at.set(if minutes == 0 { None } else { Some(Instant::now() + Duration::from_secs(minutes * 60)) }); state.notify(if minutes == 0 { "Temporizador desactivado".into() } else { format!("La reproducción se detendrá en {minutes} minutos") }); } }, if minutes == 0 { "No" } else { "{minutes} min" } }
                        } }
                    }
                }
            }
            button { class: if *s.queue_open.read() { "player-btn player-btn-small queue-button player-btn-active" } else { "player-btn player-btn-small queue-button" }, title: "Cola de reproducción", aria_expanded: *s.queue_open.read(), onclick: move |_| queue_state.queue_open.toggle(), Icon { name: "playlist", size: 15 }
                if !s.queue.read().is_empty() && !*s.queue_open.read() { span { class: "queue-dot" } }
            }
        }
    } }
}

#[component]
pub fn VideoPanel() -> Element {
    let mut s = use_context::<AppState>();
    let Some(track) = s.video_track.read().clone() else {
        return rsx! {};
    };
    let mut close = s.clone();
    let ended = s.clone();
    let error = s.clone();
    let mut native_play = s.clone();
    let mut native_pause = s.clone();
    let mut generation = use_signal(|| 0u64);
    rsx! { div { class: "modal-backdrop", onclick: move |_| { close.video_track.set(None); close.playing.set(false); },
        div { class: "modal-panel", style: "width:min(1000px,94vw);max-width:1000px;background:#111", role: "dialog", aria_label: "Reproductor de vídeo", onclick: |e| e.stop_propagation(),
            div { class: "modal-header", div { class: "modal-title", "{track.title}" }
                button { class: "btn-icon-sm", title: "Cerrar vídeo", onclick: move |_| { s.video_track.set(None); s.playing.set(false); }, Icon { name: "close" } }
            }
            video { key: "{track.id}-{generation}", src: crate::local_media::media_url(&track.id), controls: true, autoplay: true, playsinline: true, style: "width:100%;max-height:70vh;display:block",
                onplay: move |_| native_play.playing.set(true),
                onpause: move |_| native_pause.playing.set(false),
                onended: move |_| { let advances = *ended.repeat.read() > 0 || !ended.queue.read().is_empty(); if advances { generation += 1; } ended.next(true); },
                onerror: move |_| error.notify("El sistema no puede reproducir este formato de vídeo."),
            }
        }
    } }
}

#[component]
pub fn QueuePanel() -> Element {
    let mut s = use_context::<AppState>();
    let mut clear_state = s.clone();
    let current = s.current.read().clone();
    let queue = s.queue.read().clone();
    rsx! { aside { class: "queue-panel", aria_label: "Cola de reproducción",
        div { class: "queue-panel__header", h2 { class: "queue-panel__title", "A continuación" }
            button { class: "btn-icon-sm", title: "Vaciar cola", onclick: move |_| { clear_state.queue.write().clear(); clear_state.persist_playback(); }, Icon { name: "trash", size: 14 } }
            button { class: "btn-icon-sm", title: "Cerrar cola", onclick: move |_| s.queue_open.set(false), Icon { name: "close", size: 14 } }
        }
        div { class: "queue-panel__scroll",
            if let Some(track) = current {
                div { class: "queue-label", "Ahora suena" }
                div { class: "row row--active", Thumb { src: crate::app::artwork_src(&track), size: "xs" } div { class: "row__meta", div { class: "row__title", "{track.title}" } div { class: "row__sub", "{track.artist}" } }
                    if *s.playing.read() { Equalizer {} }
                }
            }
            div { class: "queue-label", "A continuación · {queue.len()}" }
            if queue.is_empty() { p { class: "rust-settings-note", "Añade canciones desde sus opciones." } }
            for (index, track) in queue.into_iter().enumerate() { QueueRow { key: "{index}-{track.id}", track, index } }
        }
    } }
}

#[component]
fn QueueRow(track: Track, index: usize) -> Element {
    let s = use_context::<AppState>();
    let mut play_state = s.clone();
    let play_track = track.clone();
    let mut up_state = s.clone();
    let mut remove_state = s.clone();
    rsx! { div { class: "row",
        button { class: "rust-track-main", onclick: move |_| {
            let mut tracks = play_state.queue.read().clone(); if index < tracks.len() { tracks.remove(index); }
            play_state.queue.set(tracks); play_state.play(play_track.clone());
        }, Thumb { src: crate::app::artwork_src(&track), size: "xs" } div { class: "row__meta", div { class: "row__title", "{track.title}" } div { class: "row__sub", "{track.artist}" } } }
        button { class: "btn-icon-sm", title: "Mover arriba", disabled: index == 0, onclick: move |_| {
            { let mut queue = up_state.queue.write(); if index > 0 && index < queue.len() { queue.swap(index, index - 1); } }
            up_state.persist_playback();
        }, "↑" }
        button { class: "btn-icon-sm", title: "Quitar de la cola", onclick: move |_| {
            { let mut queue = remove_state.queue.write(); if index < queue.len() { queue.remove(index); } }
            remove_state.persist_playback();
        }, Icon { name: "close", size: 12 } }
    } }
}
