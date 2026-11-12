//! What the player shows of the DJ: before a mix, a chip naming the planned
//! technique and when it starts, and an optional zone on the progress bar
//! where the songs will sound together; during a blend, a badge with the
//! technique, a level meter and a small crossfader whose knob follows the
//! blend, a light that sweeps along the progress bar, and the blend's show:
//! in the player bar a beam along its top edge, a halo around the cover and
//! the leaving song's cover sliding away behind it; in the full-screen view
//! turning rays, rings that leave the cover, the leaving cover sliding off
//! it and a banner with a crossfader from one song to the other. Between
//! beat-matched songs the show pulses on their beat. An «IA» tag marks the
//! mixes the vocal model shaped. Styles in `dj_ui.css`, included once at the
//! root. See `docs/systems/Dj.md`.

use crate::app::{artwork_src, Thumb};
use crate::dj::{BlendScene, MixName, NextMix};
use crate::state::time_label;
use dioxus::prelude::*;
use yt_core::Track;
use yt_media::Blend;

/// Travel of the crossfader knob, in pixels (track width minus knob width).
const FADER_TRAVEL_PX: f32 = 16.0;
/// Bars of a blend's level meter.
const METER_BARS: usize = 4;
/// Rings that leave the full-screen cover: one starts on every beat, and a
/// wider one on every fourth.
const STAGE_RINGS: usize = 3;
/// How strongly a blend's show appears at its start and its end, of the 1 it
/// reaches in its middle.
const RESTING_ENERGY: f32 = 0.4;
/// How far the leaving cover has moved when the blend ends, behind the
/// player bar's cover and off the full-screen one: a share of its own width,
/// a turn in degrees and the share of its size it loses.
const BAR_LEAVE: (f32, f32, f32) = (42.0, 14.0, 0.2);
const STAGE_LEAVE: (f32, f32, f32) = (46.0, 9.0, 0.12);

/// The badge beside the playing song's title while a blend is heard; `name`
/// is the planned mix being played, if the blend is one.
#[component]
pub(crate) fn BlendBadge(blend: Blend, name: Option<MixName>) -> Element {
    let name = name.filter(|_| blend.mix);
    let label = name.map_or_else(|| crate::dj::blend_label(blend), |name| name.label);
    let ai = name.is_some_and(|name| name.ai);
    let offset = blend.progress.clamp(0.0, 1.0) * FADER_TRAVEL_PX;
    rsx! {
        span {
            class: "dj-blend",
            role: "status",
            title: "{label}",
            Meter {}
            span {
                class: "dj-blend__fader",
                aria_hidden: "true",
                span {
                    class: "dj-blend__knob",
                    style: "transform: translateX({offset:.1}px)",
                }
            }
            "{label}"
            AiTag {
                shown: ai,
            }
        }
    }
}

/// The chip beside the playing song's title that names the mix planned out
/// of it and the second of the song where it starts.
#[component]
pub(crate) fn NextMixChip(next: NextMix) -> Element {
    let label = next.name.label;
    let start = time_label(next.start);
    let (text, title) = if next.preparing {
        (
            format!("Preparando {label}"),
            format!("La IA está preparando la mezcla de {start}"),
        )
    } else {
        (
            label.to_owned(),
            format!("Próxima mezcla: {label}, empieza en {start}"),
        )
    };
    rsx! {
        span {
            class: "dj-next",
            class: if next.preparing { "dj-next--preparing" },
            title: "{title}",
            aria_label: "{title}",
            span {
                class: "dj-next__dot",
                aria_hidden: "true",
            }
            span {
                class: "dj-next__label",
                "{text}"
            }
            span {
                class: "dj-next__time",
                "· {start}"
            }
            AiTag {
                shown: next.name.ai,
            }
        }
    }
}

/// The «IA» tag of a mix the vocal model shaped; renders nothing otherwise.
#[component]
fn AiTag(shown: bool) -> Element {
    rsx! {
        if shown {
            span {
                class: "dj-ai",
                title: "Planeada con la IA de voces",
                "IA"
            }
        }
    }
}

/// Where the planned mix sounds on the progress bar, as percentages of the
/// song: its start and its width. `None` without a duration.
pub(crate) fn mix_zone(next: NextMix, duration: f64) -> Option<(f64, f64)> {
    if !(duration.is_finite() && duration > 0.0) {
        return None;
    }
    let start = (next.start / duration).clamp(0.0, 1.0);
    let end = (next.end / duration).clamp(start, 1.0);
    Some((start * 100.0, (end - start) * 100.0))
}

/// The zone of the progress bar where the planned mix sounds, with a tick
/// where it starts. Geometry from `mix_zone`.
#[component]
pub(crate) fn MixZone(left: f64, width: f64, preparing: bool) -> Element {
    rsx! {
        span {
            class: "dj-zone",
            class: if preparing { "dj-zone--preparing" },
            aria_hidden: "true",
            style: "left:{left:.2}%;width:{width:.2}%",
        }
    }
}

/// How strongly a blend's show appears at `progress`: it swells from
/// [`RESTING_ENERGY`] into the middle of the blend and settles as it ends.
pub(crate) fn energy(progress: f32) -> f32 {
    let swell = (std::f32::consts::PI * progress.clamp(0.0, 1.0))
        .sin()
        .max(0.0);
    RESTING_ENERGY + (1.0 - RESTING_ENERGY) * swell
}

/// The custom properties that put a blend's show on its music: `--dj-beat`,
/// a beat of it, and `--dj-phase`, how far into a beat the blend is, as a
/// negative delay. Empty unless the songs are beat-matched and already sound
/// together; the show then breathes slowly instead (`dj_ui.css`).
fn beat_timing(blend: Blend, scene: &BlendScene) -> String {
    match scene.beat_seconds {
        Some(beat) if blend.mix && blend.progress > 0.0 && beat > 0.0 => {
            let into = (blend.progress.clamp(0.0, 1.0) * scene.seconds) % beat;
            format!("--dj-beat:{beat:.3}s;--dj-phase:-{into:.3}s;")
        }
        _ => String::new(),
    }
}

/// One group of a blend's show. Its timing is taken once, when it mounts, so
/// its pulses keep their place on the beat while `--dj-energy` follows the
/// blend; a caller mounts it only once the songs sound together.
#[component]
fn Show(
    blend: Blend,
    scene: BlendScene,
    class: &'static str,
    playing: bool,
    children: Element,
) -> Element {
    let timing = use_hook(|| beat_timing(blend, &scene));
    let energy = energy(blend.progress);
    rsx! {
        span {
            class: "dj-show {class}",
            class: if !timing.is_empty() { "dj-show--beat" },
            class: if !playing { "dj-show--paused" },
            aria_hidden: "true",
            style: "{timing}--dj-energy:{energy:.2}",
            {children}
        }
    }
}

/// Small bars that move while two songs blend.
#[component]
fn Meter() -> Element {
    rsx! {
        span {
            class: "dj-meter",
            aria_hidden: "true",
            for _ in 0..METER_BARS {
                span {
                    class: "dj-meter__bar",
                }
            }
        }
    }
}

/// The player bar's light while two songs sound together: a glow behind it
/// and a beam along its top edge that a spark runs along.
#[component]
pub(crate) fn BlendBeam(blend: Blend, scene: BlendScene, playing: bool) -> Element {
    rsx! {
        if blend.progress > 0.0 {
            Show {
                blend,
                scene,
                class: "dj-beam",
                playing,
                span {
                    class: "dj-beam__glow",
                }
                span {
                    class: "dj-beam__line",
                }
                span {
                    class: "dj-beam__spark",
                }
            }
        }
    }
}

/// A halo around the player bar's cover while two songs sound together: a
/// light that turns around its edge and a ring that leaves it on the beat.
#[component]
pub(crate) fn BlendHalo(blend: Blend, scene: BlendScene, playing: bool) -> Element {
    rsx! {
        if blend.progress > 0.0 {
            Show {
                blend,
                scene,
                class: "dj-halo",
                playing,
                span {
                    class: "dj-halo__ring",
                }
                span {
                    class: "dj-halo__edge",
                    span {
                        class: "dj-halo__turn",
                    }
                }
            }
        }
    }
}

/// The cover of the song that leaves, moving away as the blend goes on:
/// from behind the player bar's cover, or (`stage`) off the full-screen one,
/// which it covers when the blend starts.
#[component]
pub(crate) fn LeavingCover(from: Track, progress: f32, stage: bool) -> Element {
    let progress = progress.clamp(0.0, 1.0);
    let (slide, turn, shrink) = if stage { STAGE_LEAVE } else { BAR_LEAVE };
    let opacity = 1.0 - progress;
    let (x, angle, scale) = (-slide * progress, -turn * progress, 1.0 - shrink * progress);
    let size = if stage { "full" } else { "md" };
    rsx! {
        span {
            class: if stage { "dj-leaving dj-leaving--stage" } else { "dj-leaving dj-leaving--bar" },
            aria_hidden: "true",
            style: "opacity:{opacity:.3};transform:translateX({x:.1}%) rotate({angle:.1}deg) scale({scale:.3})",
            Thumb {
                src: artwork_src(&from),
                size,
                video: false,
            }
        }
    }
}

/// Behind the full-screen view while two songs sound together: rays that
/// turn around its middle and a glow on its edges.
#[component]
pub(crate) fn BlendStage(blend: Blend, scene: BlendScene, playing: bool) -> Element {
    rsx! {
        if blend.progress > 0.0 {
            Show {
                blend,
                scene,
                class: "dj-stage",
                playing,
                span {
                    class: "dj-stage__rays",
                }
                span {
                    class: "dj-stage__frame",
                }
            }
        }
    }
}

/// Rings that leave the full-screen cover while two songs sound together.
#[component]
pub(crate) fn BlendRings(blend: Blend, scene: BlendScene, playing: bool) -> Element {
    rsx! {
        if blend.progress > 0.0 {
            Show {
                blend,
                scene,
                class: "dj-rings",
                playing,
                for _ in 0..STAGE_RINGS {
                    span {
                        class: "dj-rings__ring",
                    }
                }
            }
        }
    }
}

/// The banner of the blend in the full-screen view: what it is, and a
/// crossfader that travels from the leaving song `from` to the playing one
/// `to`; `name` is the planned mix being played, if the blend is one.
#[component]
pub(crate) fn BlendBanner(
    blend: Blend,
    name: Option<MixName>,
    from: String,
    to: String,
) -> Element {
    let name = name.filter(|_| blend.mix);
    let label = name.map_or_else(|| crate::dj::blend_label(blend), |name| name.label);
    let ai = name.is_some_and(|name| name.ai);
    let progress = blend.progress.clamp(0.0, 1.0);
    let travel = progress * 100.0;
    // Each title is brightest while its song leads.
    let (leaving, arriving) = (1.0 - 0.6 * progress, 0.4 + 0.6 * progress);
    rsx! {
        div {
            class: "dj-banner",
            role: "status",
            aria_label: "{label}: de {from} a {to}",
            div {
                class: "dj-banner__name",
                Meter {}
                "{label}"
                AiTag {
                    shown: ai,
                }
            }
            div {
                class: "dj-banner__songs",
                span {
                    class: "dj-banner__song",
                    style: "opacity:{leaving:.2}",
                    title: "{from}",
                    "{from}"
                }
                span {
                    class: "dj-banner__song dj-banner__song--to",
                    style: "opacity:{arriving:.2}",
                    title: "{to}",
                    "{to}"
                }
            }
            span {
                class: "dj-banner__fader",
                aria_hidden: "true",
                span {
                    class: "dj-banner__fill",
                    style: "transform:scaleX({progress:.3})",
                }
                span {
                    class: "dj-banner__carriage",
                    style: "transform:translateX({travel:.1}%)",
                    span {
                        class: "dj-banner__knob",
                    }
                }
            }
        }
    }
}

/// A light that sweeps along the progress bar's fill during a blend.
#[component]
pub(crate) fn BlendSweep() -> Element {
    rsx! {
        span {
            class: "dj-sweep",
            aria_hidden: "true",
            span {
                class: "dj-sweep__light",
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::render;

    const MASHUP: MixName = MixName {
        label: "Mashup",
        ai: true,
    };

    fn badge() -> Element {
        rsx! {
            BlendBadge {
                blend: Blend {
                    mix: true,
                    progress: 0.5,
                },
                name: None,
            }
        }
    }

    fn mashup_badge() -> Element {
        rsx! {
            BlendBadge {
                blend: Blend {
                    mix: true,
                    progress: 0.25,
                },
                name: Some(MASHUP),
            }
        }
    }

    fn chips() -> Element {
        let next = NextMix {
            start: 192.0,
            end: 208.0,
            name: MASHUP,
            preparing: false,
        };
        let plain = NextMix {
            name: MixName {
                label: "Doble drop",
                ai: false,
            },
            preparing: true,
            ..next
        };
        rsx! {
            NextMixChip {
                next,
            }
            NextMixChip {
                next: plain,
            }
        }
    }

    #[test]
    fn the_badge_names_the_blend_and_moves_its_knob_with_the_progress() {
        let html = render(badge, |_| {});
        assert!(html.contains("Mezclando"), "{html}");
        assert!(html.contains("translateX(8.0px)"), "{html}");
        assert!(!html.contains(">IA<"), "{html}");
    }

    #[test]
    fn the_badge_names_a_planned_mix_and_tags_the_ai() {
        let html = render(mashup_badge, |_| {});
        assert!(
            html.contains("Mashup") && !html.contains("Mezclando"),
            "{html}"
        );
        assert!(html.contains(">IA<"), "{html}");
    }

    #[test]
    fn the_chip_says_what_mix_comes_and_when() {
        let html = render(chips, |_| {});
        assert!(
            html.contains(">Mashup<") && html.contains("· 3:12"),
            "{html}"
        );
        assert!(html.contains("Preparando Doble drop"), "{html}");
        assert_eq!(html.matches(">IA<").count(), 1, "{html}");
    }

    fn scene(beat_seconds: Option<f32>) -> BlendScene {
        BlendScene {
            from: crate::test_support::remote_track("aqz-KE-bpKQ", "La que sale"),
            seconds: 16.0,
            beat_seconds,
        }
    }

    const HALFWAY: Blend = Blend {
        mix: true,
        progress: 0.5,
    };

    fn bar_show() -> Element {
        rsx! {
            BlendBeam {
                blend: HALFWAY,
                scene: scene(Some(0.5)),
                playing: true,
            }
            BlendHalo {
                blend: HALFWAY,
                scene: scene(Some(0.5)),
                playing: false,
            }
            LeavingCover {
                from: scene(None).from,
                progress: 0.5,
                stage: false,
            }
        }
    }

    fn stage_show() -> Element {
        rsx! {
            BlendStage {
                blend: HALFWAY,
                scene: scene(Some(0.5)),
                playing: true,
            }
            BlendRings {
                blend: HALFWAY,
                scene: scene(None),
                playing: true,
            }
            BlendBanner {
                blend: HALFWAY,
                name: Some(MASHUP),
                from: "La que sale",
                to: "La que entra",
            }
        }
    }

    fn waiting_show() -> Element {
        let waiting = Blend {
            mix: true,
            progress: 0.0,
        };
        rsx! {
            BlendBeam {
                blend: waiting,
                scene: scene(Some(0.5)),
                playing: true,
            }
            BlendStage {
                blend: waiting,
                scene: scene(Some(0.5)),
                playing: true,
            }
            BlendRings {
                blend: waiting,
                scene: scene(Some(0.5)),
                playing: true,
            }
        }
    }

    #[test]
    fn the_show_pulses_on_the_beat_of_matched_songs() {
        let blend = Blend {
            mix: true,
            progress: 0.25,
        };
        // 4.2 s into the blend with a beat every 0.5 s: 0.2 s into a beat.
        let timed = BlendScene {
            seconds: 16.8,
            ..scene(Some(0.5))
        };
        assert_eq!(
            beat_timing(blend, &timed),
            "--dj-beat:0.500s;--dj-phase:-0.200s;"
        );
        assert_eq!(beat_timing(blend, &scene(None)), "");
        let crossfade = Blend {
            mix: false,
            ..blend
        };
        assert_eq!(beat_timing(crossfade, &timed), "");
        let waiting = Blend {
            progress: 0.0,
            ..blend
        };
        assert_eq!(beat_timing(waiting, &timed), "", "not before they sound");
        assert!((energy(0.0) - RESTING_ENERGY).abs() < 1e-6);
        assert!((energy(0.5) - 1.0).abs() < 1e-6);
        assert!((energy(1.0) - RESTING_ENERGY).abs() < 1e-3);
        assert!(energy(0.25) > energy(0.1) && energy(0.75) > energy(0.9));
    }

    #[test]
    fn the_player_bar_lights_up_while_two_songs_sound_together() {
        let html = render(bar_show, |_| {});
        assert!(html.contains("dj-show dj-beam dj-show--beat"), "{html}");
        assert!(
            html.contains("--dj-beat:0.500s;--dj-phase:-0.000s;--dj-energy:1.00"),
            "{html}"
        );
        assert!(
            html.contains("dj-show dj-halo dj-show--beat dj-show--paused"),
            "{html}"
        );
        assert!(
            html.contains("dj-halo__turn") && html.contains("dj-beam__spark"),
            "{html}"
        );
        // Halfway, the leaving cover is half gone.
        assert!(
            html.contains(
                "opacity:0.500;transform:translateX(-21.0%) rotate(-7.0deg) scale(0.900)"
            ),
            "{html}"
        );
        assert!(html.contains("dj-leaving--bar"), "{html}");
        let waiting = render(waiting_show, |_| {});
        assert!(
            !waiting.contains("dj-show"),
            "nothing before the songs meet: {waiting}"
        );
    }

    #[test]
    fn the_full_screen_show_names_the_blend_and_both_songs() {
        let html = render(stage_show, |_| {});
        assert!(
            html.contains("dj-stage__rays") && html.contains("dj-stage__frame"),
            "{html}"
        );
        assert_eq!(
            html.matches("dj-rings__ring").count(),
            STAGE_RINGS,
            "{html}"
        );
        // Without a shared beat the rings breathe on the default timing.
        assert!(
            html.contains("style=\"--dj-energy:1.00\"><span class=\"dj-rings__ring\""),
            "{html}"
        );
        assert!(
            html.contains("aria-label=\"Mashup: de La que sale a La que entra\""),
            "{html}"
        );
        assert!(
            html.contains(">La que sale<") && html.contains(">La que entra<"),
            "{html}"
        );
        assert!(html.contains(">IA<"), "{html}");
        assert!(html.contains("transform:translateX(50.0%)"), "{html}");
        assert!(html.contains("transform:scaleX(0.500)"), "{html}");
        assert_eq!(html.matches("dj-meter__bar").count(), METER_BARS, "{html}");
    }

    #[test]
    fn the_zone_covers_the_overlap_inside_the_bar() {
        let next = NextMix {
            start: 150.0,
            end: 225.0,
            name: MASHUP,
            preparing: false,
        };
        assert_eq!(mix_zone(next, 300.0), Some((50.0, 25.0)));
        let late = NextMix { end: 320.0, ..next };
        assert_eq!(mix_zone(late, 300.0), Some((50.0, 50.0)));
        assert_eq!(mix_zone(next, 0.0), None);
        assert_eq!(mix_zone(next, f64::NAN), None);
    }
}
