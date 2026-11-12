//! Upcoming concerts of an artist, the ones in the user's region first, with
//! ticket links opened in the browser. Which events are near is decided by
//! `yt_core::RegionPreferences::matches`; showing concerts starts the
//! session's zone detection (`crate::region`). See `docs/systems/Browse.md`
//! and `docs/systems/Region.md`.

use crate::{
    icons::Icon,
    region::Detection,
    state::{AppState, Page},
};
use dioxus::prelude::*;
use yt_core::RegionPreferences;
use yt_media::LiveEvent;

/// Concerts listed on the artist page.
const SHOWN_EVENTS: usize = 8;
/// Web search opened when YouTube lists no concerts for the artist.
const CONCERT_SEARCH: &str = "https://www.google.com/search";

/// `events` with the ones near the user first, each flagged; provider (date)
/// order is kept within both groups.
pub(crate) fn nearby_first(
    events: &[LiveEvent],
    region: &RegionPreferences,
) -> Vec<(LiveEvent, bool)> {
    let mut flagged: Vec<(LiveEvent, bool)> = events
        .iter()
        .map(|event| {
            let near = region.matches(&event.location) || region.matches(&event.venue);
            (event.clone(), near)
        })
        .collect();
    flagged.sort_by_key(|(_, near)| !near);
    flagged
}

/// The place of the region, city first.
fn place(region: &RegionPreferences) -> &str {
    if region.city.trim().is_empty() {
        region.country.trim()
    } else {
        region.city.trim()
    }
}

/// A web search for concerts of `artist` near the user, with the query
/// encoded as a URL parameter.
pub(crate) fn concert_search_url(artist: &str, region: &RegionPreferences) -> String {
    let query = format!("conciertos {} {}", artist.trim(), place(region));
    url::Url::parse_with_params(CONCERT_SEARCH, [("q", query.trim())])
        .map(String::from)
        .unwrap_or_default()
}

#[component]
pub fn ConcertsSection(artist: String, events: Vec<LiveEvent>) -> Element {
    let state = use_context::<AppState>();
    let detector = state.clone();
    use_effect(move || detector.detect_region_once());
    let region = state.settings.read().region.clone();
    let detection = *state.region.detection.read();
    let listed = nearby_first(&events, &region);
    let near = listed.iter().filter(|(_, near)| *near).count();
    let title = if near > 0 {
        "Conciertos cerca de ti"
    } else {
        "Próximos conciertos"
    };
    let counts = match (near, listed.len()) {
        (0, 0) => String::new(),
        (0, 1) => "1 fecha anunciada".to_owned(),
        (0, total) => format!("{total} fechas anunciadas"),
        (1, _) => format!("1 fecha cerca de {}", place(&region)),
        (count, _) => format!("{count} fechas cerca de {}", place(&region)),
    };
    let subtitle = match detection.message() {
        Some(message) if detection == Detection::Detecting && near == 0 && !listed.is_empty() => {
            format!("{counts} · {message}")
        }
        _ => counts,
    };
    let denied = region.detect && detection == Detection::Denied;
    let hint = if region.is_set() {
        format!("Busca fechas cerca de {} en la web.", place(&region))
    } else if region.detect && detection == Detection::Detecting {
        "Buscando tu zona para mostrarte primero los conciertos cerca de ti…".to_owned()
    } else if denied {
        "Permite el acceso a tu ubicación o indica tu ciudad en Ajustes para ver primero los conciertos cerca de ti.".to_owned()
    } else {
        "Indica tu ciudad en Ajustes para ver primero los conciertos cerca de ti.".to_owned()
    };
    let search = concert_search_url(&artist, &region);
    let open_search = state.clone();
    let settings = state.clone();
    let permissions = state.clone();
    let mut settings_section = state.settings_section;
    rsx! {
        section { class: "concerts", aria_label: "{title}",
            div { class: "browse-shelf__header",
                div { class: "browse-shelf__titles",
                    h2 { class: "browse-shelf__title", "{title}" }
                    if !subtitle.is_empty() {
                        p { class: "browse-shelf__sub", "{subtitle}" }
                    }
                }
            }
            if listed.is_empty() {
                div { class: "concerts-empty",
                    span { class: "concerts-empty__icon", Icon { name: "ticket", size: 20 } }
                    div { class: "concerts-empty__text",
                        span { class: "concerts-empty__title", "YouTube no anuncia conciertos de {artist}" }
                        span { class: "concerts-empty__hint", "{hint}" }
                    }
                    div { class: "concerts-empty__actions",
                        button {
                            class: "btn-secondary",
                            onclick: move |_| crate::native::open_url(&open_search, &search),
                            Icon { name: "external", size: 13 }
                            "Buscar conciertos"
                        }
                        if denied && !region.is_set() {
                            button {
                                class: "btn-secondary",
                                onclick: move |_| crate::native::open_location_settings(&permissions),
                                Icon { name: "settings", size: 13 }
                                "Permitir ubicación"
                            }
                        }
                        if !region.is_set() {
                            button {
                                class: "btn-secondary",
                                onclick: move |_| {
                                    settings_section.set("region".into());
                                    settings.navigate(Page::Settings);
                                },
                                Icon { name: "map-pin", size: 13 }
                                "Indicar mi ciudad"
                            }
                        }
                    }
                }
            } else {
                div { class: "concerts-grid",
                    for (index, (event, near)) in listed.into_iter().take(SHOWN_EVENTS).enumerate() {
                        article {
                            key: "{index}-{event.url}",
                            class: if near { "concert concert--near" } else { "concert" },
                            div { class: "concert__date",
                                Icon { name: "calendar", size: 14 }
                                span { "{event.date_text}" }
                            }
                            span { class: "concert__title", title: "{event.title}", "{event.title}" }
                            span { class: "concert__place",
                                Icon { name: "map-pin", size: 12 }
                                span {
                                    "{event.venue}"
                                    if !event.venue.is_empty() && !event.location.is_empty() { " · " }
                                    "{event.location}"
                                }
                            }
                            div { class: "concert__footer",
                                if near {
                                    span { class: "concert__badge", "Cerca de ti" }
                                }
                                button {
                                    class: "btn-accent concert__tickets",
                                    onclick: {
                                        let state = state.clone();
                                        let url = event.url.clone();
                                        move |_| crate::native::open_url(&state, &url)
                                    },
                                    Icon { name: "ticket", size: 13 }
                                    "Boletos"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(title: &str, location: &str) -> LiveEvent {
        LiveEvent {
            title: title.into(),
            date_text: "22 may".into(),
            venue: "Arena".into(),
            location: location.into(),
            url: "https://www.ticketmaster.com.mx/event".into(),
        }
    }

    #[test]
    fn nearby_concerts_come_first_in_their_original_order() {
        let region = RegionPreferences {
            city: "Monterrey".into(),
            country: "México".into(),
            ..RegionPreferences::default()
        };
        let events = [
            event("Madrid", "Madrid, España"),
            event("MTY 1", "Monterrey, NL"),
            event("CDMX", "Ciudad de México"),
            event("MTY 2", "Monterrey, Nuevo León"),
        ];
        let listed = nearby_first(&events, &region);
        let titles: Vec<(&str, bool)> = listed
            .iter()
            .map(|(event, near)| (event.title.as_str(), *near))
            .collect();
        assert_eq!(
            titles,
            [
                ("MTY 1", true),
                ("MTY 2", true),
                ("Madrid", false),
                ("CDMX", false)
            ]
        );
    }

    #[test]
    fn concert_searches_encode_artist_and_place() {
        let region = RegionPreferences {
            country: "México".into(),
            ..RegionPreferences::default()
        };
        let url = concert_search_url("Peso Pluma & Friends", &region);
        assert!(url.starts_with("https://www.google.com/search?q="));
        assert!(url.contains("Peso+Pluma+%26+Friends"));
        assert!(!url.contains(' '));
    }

    fn no_concerts() -> Element {
        rsx! {
            ConcertsSection { artist: "Morat", events: Vec::new() }
        }
    }

    #[test]
    fn concerts_say_how_the_zone_detection_goes() {
        let detecting = crate::test_support::render(no_concerts, |state| {
            state.region.detection.set(Detection::Detecting)
        });
        assert!(detecting.contains("Buscando tu zona para mostrarte"));
        assert!(!detecting.contains("Permitir ubicación"));
        let denied = crate::test_support::render(no_concerts, |state| {
            state.region.detection.set(Detection::Denied)
        });
        assert!(denied.contains("Permite el acceso a tu ubicación"));
        assert!(denied.contains("Permitir ubicación"));
        assert!(denied.contains("Indicar mi ciudad"));
        let detected = crate::test_support::render(no_concerts, |state| {
            state.region.detection.set(Detection::Detected);
            state.edit_settings(|settings| {
                settings.region.city = "Monterrey".into();
                settings.region.country = "México".into();
            });
        });
        assert!(detected.contains("Busca fechas cerca de Monterrey en la web."));
        assert!(!detected.contains("Indicar mi ciudad"));
    }
}
