//! Shared visual building blocks for browse pages: shelves, cover cards,
//! artist circles, generated animated covers and detail heroes.
//! See `docs/systems/Browse.md`.

use crate::{app::Thumb, icons::Icon};
use dioxus::prelude::*;

/// Stable seed for a generated cover, from any key (FNV-1a).
pub(crate) fn cover_seed(key: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    key.bytes().fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    })
}

/// CSS custom properties of a generated cover; colors come from `yt-theme`.
pub(crate) fn cover_style(key: &str) -> String {
    let [main, light, deep] = yt_theme::generated_palette(cover_seed(key));
    format!(
        "--cover-1:{};--cover-2:{};--cover-3:{}",
        main.to_hex(),
        light.to_hex(),
        deep.to_hex()
    )
}

/// A titled, horizontally scrolling row.
#[component]
pub fn Shelf(
    title: String,
    #[props(default)] subtitle: String,
    #[props(default)] action: Option<(String, EventHandler<()>)>,
    children: Element,
) -> Element {
    rsx! {
        section { class: "browse-shelf", aria_label: "{title}",
            div { class: "browse-shelf__header",
                div { class: "browse-shelf__titles",
                    h2 { class: "browse-shelf__title", "{title}" }
                    if !subtitle.is_empty() {
                        div { class: "browse-shelf__sub", "{subtitle}" }
                    }
                }
                if let Some((label, handler)) = action {
                    button { class: "section-action", onclick: move |_| handler.call(()), "{label}" Icon { name: "chevron-right", size: 12 } }
                }
            }
            div { class: "browse-shelf__scroller", {children} }
        }
    }
}

/// Animated generated artwork for collections and stations: drifting blurred
/// color fields in the cover's palette, with the title set on it.
#[component]
pub fn GeneratedCover(
    cover: String,
    title: String,
    #[props(default)] eyebrow: String,
    #[props(default = "music")] icon: &'static str,
) -> Element {
    rsx! {
        div { class: "generated-cover", style: "{cover_style(&cover)}",
            span { class: "generated-cover__blob generated-cover__blob--a" }
            span { class: "generated-cover__blob generated-cover__blob--b" }
            span { class: "generated-cover__blob generated-cover__blob--c" }
            span { class: "generated-cover__grain" }
            div { class: "generated-cover__content",
                span { class: "generated-cover__icon", Icon { name: icon, size: 18, stroke_width: 2.0 } }
                if !eyebrow.is_empty() {
                    span { class: "generated-cover__eyebrow", "{eyebrow}" }
                }
                span { class: "generated-cover__title", "{title}" }
            }
        }
    }
}

/// Square card: artwork (or a generated cover) with a title and subtitle.
#[component]
pub fn CoverCard(
    title: String,
    #[props(default)] subtitle: String,
    #[props(default)] artwork: String,
    /// Generated cover key when there is no artwork.
    #[props(default)]
    generated: Option<String>,
    #[props(default = "music")] icon: &'static str,
    #[props(default = false)] round: bool,
    #[props(default = false)] active: bool,
    onopen: EventHandler<()>,
    #[props(default)] onplay: Option<EventHandler<()>>,
    #[props(default)] oncontext: Option<EventHandler<MouseEvent>>,
    #[props(default)] children: Element,
) -> Element {
    let class = match (round, active) {
        (true, _) => "cover-card cover-card--round",
        (false, true) => "cover-card cover-card--active",
        (false, false) => "cover-card",
    };
    rsx! {
        div { class,
            button {
                class: "cover-card__open",
                title: "{title}",
                onclick: move |_| onopen.call(()),
                oncontextmenu: move |event| {
                    if let Some(handler) = &oncontext {
                        handler.call(event);
                    }
                },
                div { class: "cover-card__art",
                    match &generated {
                        Some(key) if artwork.is_empty() => rsx! {
                            GeneratedCover { cover: key.clone(), title: title.clone(), icon }
                        },
                        _ => rsx! {
                            Thumb { src: artwork.clone(), size: "square", icon_size: Some(30) }
                        },
                    }
                }
                span { class: "cover-card__title", "{title}" }
                if !subtitle.is_empty() {
                    span { class: "cover-card__sub", "{subtitle}" }
                }
            }
            if let Some(play) = onplay {
                button {
                    class: "cover-card__play",
                    title: "Reproducir",
                    aria_label: "Reproducir {title}",
                    onclick: move |event| {
                        event.stop_propagation();
                        play.call(());
                    },
                    Icon { name: "play", size: 14, fill: "currentColor", style: "margin-left:2px" }
                }
            }
            {children}
        }
    }
}

/// Large hero for artist, album and collection pages: blurred artwork glow,
/// big cover, eyebrow, title, facts and actions. Real artwork colors the
/// glow and the action buttons; without it the generated cover colors do. A
/// `banner` (an artist's wide photo) fills the whole hero instead of the
/// cover.
#[component]
pub fn DetailHero(
    title: String,
    #[props(default)] eyebrow: String,
    #[props(default)] facts: Vec<String>,
    #[props(default)] artwork: String,
    #[props(default)] banner: String,
    #[props(default)] generated: Option<String>,
    #[props(default = false)] round: bool,
    #[props(default = "music")] icon: &'static str,
    onback: EventHandler<()>,
    #[props(default)] children: Element,
) -> Element {
    let artwork_tokens = crate::theme::use_artwork_tokens(artwork.clone());
    let style = if artwork.is_empty() {
        generated.as_deref().map(cover_style).unwrap_or_default()
    } else {
        artwork_tokens
    };
    let class = if banner.is_empty() {
        "detail-hero"
    } else {
        "detail-hero detail-hero--banner"
    };
    rsx! {
        header { class, style: "{style}",
            if banner.is_empty() {
                div { class: "detail-hero__backdrop", aria_hidden: "true",
                    if !artwork.is_empty() {
                        Thumb { src: artwork.clone(), size: "square" }
                    }
                }
            } else {
                div { class: "detail-hero__banner", aria_hidden: "true",
                    Thumb { src: banner.clone(), size: "full" }
                }
            }
            div { class: "detail-hero__shade", aria_hidden: "true" }
            button { class: "detail-hero__back", title: "Atrás", aria_label: "Atrás", onclick: move |_| onback.call(()),
                Icon { name: "chevron-left", size: 18 }
            }
            div { class: "detail-hero__body",
                div {
                    class: match (round, banner.is_empty()) {
                        (_, false) => "detail-hero__art detail-hero__art--hidden",
                        (true, true) => "detail-hero__art detail-hero__art--round",
                        (false, true) => "detail-hero__art",
                    },
                    match &generated {
                        Some(key) if artwork.is_empty() => rsx! {
                            GeneratedCover { cover: key.clone(), title: title.clone(), icon }
                        },
                        _ => rsx! {
                            Thumb { src: artwork.clone(), size: "square", icon_size: Some(48) }
                        },
                    }
                }
                div { class: "detail-hero__meta",
                    if !eyebrow.is_empty() {
                        span { class: "detail-hero__eyebrow", "{eyebrow}" }
                    }
                    h1 { class: "detail-hero__title", "{title}" }
                    if !facts.is_empty() {
                        div { class: "detail-hero__facts",
                            for fact in facts { span { "{fact}" } }
                        }
                    }
                    div { class: "detail-hero__actions", {children} }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_cover_styles_are_stable_per_key_and_vary_between_keys() {
        assert_eq!(cover_style("weekly"), cover_style("weekly"));
        assert_ne!(cover_style("weekly"), cover_style("loop"));
        assert!(cover_style("x").starts_with("--cover-1:#"));
    }
}
