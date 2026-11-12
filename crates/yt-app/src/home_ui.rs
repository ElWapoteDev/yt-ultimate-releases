//! Home page: what to listen to next, built from what the user listens to.
//! See `docs/systems/Discovery.md`.

use crate::{
    app::{artwork_src, EmptyState, Equalizer, Thumb},
    cards::{CoverCard, GeneratedCover},
    discovery::{Shelf, ShelfKind, SHELF_ITEMS},
    favorites_ui::FavoriteButton,
    icons::Icon,
    navigation::Detail,
    radio_ui::StationCard,
    state::{AppState, Page},
    stations::{Station, StationKind},
};
use dioxus::prelude::*;
use yt_core::Track;

/// Featured cards shown at the top of Home.
const FEATURED_CARDS: usize = 6;
/// Artist faces in a «Destacando a…» card.
const SPOTLIGHT_FACES: usize = 3;
/// Names listed in a mix caption before «y más».
const CAPTION_NAMES: usize = 3;

/// «A, B, C y más» from the distinct artists of `tracks`.
fn artists_caption(tracks: &[Track]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for track in tracks {
        let name = track.artist.trim();
        if !name.is_empty() && !names.iter().any(|known| known.eq_ignore_ascii_case(name)) {
            names.push(name);
        }
    }
    match names.len() {
        0 => String::new(),
        count if count <= CAPTION_NAMES => names.join(", "),
        _ => format!("{} y más", names[..CAPTION_NAMES].join(", ")),
    }
}

/// One featured card: a station or a collection.
#[derive(Clone, Debug, PartialEq)]
enum Featured {
    Station(Station),
    Collection(Shelf),
}

fn featured(stations: &[Station], shelves: &[Shelf]) -> Vec<Featured> {
    let mut cards = Vec::new();
    if let Some(personal) = stations
        .iter()
        .find(|station| station.kind == StationKind::Personal)
    {
        cards.push(Featured::Station(personal.clone()));
    }
    let collections = shelves.iter().filter(|shelf| shelf.kind.is_collection());
    let mut collections = collections.collect::<Vec<_>>();
    collections.sort_by_key(|shelf| match shelf.kind {
        ShelfKind::WeeklyDiscovery => 0,
        ShelfKind::OnRepeat => 1,
        ShelfKind::Replay => 2,
        ShelfKind::Mix => 3,
        ShelfKind::DayPart => 4,
        _ => 5,
    });
    let mut collections = collections.into_iter();
    if let Some(first) = collections.next() {
        cards.push(Featured::Collection(first.clone()));
    }
    if let Some(artist) = stations
        .iter()
        .find(|station| station.kind == StationKind::Artist)
    {
        cards.push(Featured::Station(artist.clone()));
    }
    cards.extend(collections.map(|shelf| Featured::Collection(shelf.clone())));
    cards.truncate(FEATURED_CARDS);
    cards
}

#[component]
pub fn HomePage() -> Element {
    let state = use_context::<AppState>();
    let page = state.page;
    let refresher = state.clone();
    use_effect(move || {
        if *page.read() == Page::Home {
            refresher.refresh_home(false);
        }
    });
    let shelves = state.discovery.shelves.read().clone();
    let stations = state.discovery.stations.read().clone();
    let loading = *state.discovery.loading.read();
    let offline = *state.discovery.offline.read();
    let online = state.settings.read().discovery.online;
    let current = state.current.read().clone();
    let library_empty = state.library.read().is_empty();
    let featured = featured(&stations, &shelves);
    let recent = shelves
        .iter()
        .find(|shelf| shelf.kind == ShelfKind::ContinueListening)
        .cloned();
    let favorites = shelves
        .iter()
        .find(|shelf| shelf.kind == ShelfKind::Favorites)
        .cloned();
    let collections_of = |wanted: fn(ShelfKind) -> bool| -> Vec<Shelf> {
        shelves
            .iter()
            .filter(|shelf| shelf.kind.is_collection() && wanted(shelf.kind))
            .cloned()
            .collect()
    };
    let collections = collections_of(|kind| !matches!(kind, ShelfKind::Mood | ShelfKind::Replay));
    let moods = collections_of(|kind| kind == ShelfKind::Mood);
    let replays = collections_of(|kind| kind == ShelfKind::Replay);
    let song_shelves: Vec<Shelf> = shelves
        .iter()
        .filter(|shelf| {
            !shelf.kind.is_collection()
                && !matches!(
                    shelf.kind,
                    ShelfKind::ContinueListening | ShelfKind::Favorites
                )
        })
        .cloned()
        .collect();
    let refresh = state.clone();
    let enable = state.clone();
    let search = state.clone();
    let open_favorites = state.clone();
    rsx! {
        div { class: "page-shell home-page",
            style { {include_str!("home_ui.css")} }
            div { class: "home-header",
                h1 { class: "home-header__title", "Inicio" }
                button {
                    class: if loading { "btn-icon home-refresh home-refresh--busy" } else { "btn-icon home-refresh" },
                    title: "Actualizar recomendaciones",
                    aria_label: "Actualizar recomendaciones",
                    disabled: loading,
                    onclick: move |_| refresh.refresh_home(true),
                    Icon { name: "refresh", size: 15 }
                }
            }
            if online && offline {
                div { class: "home-banner", role: "status",
                    span { "Sin conexión con YouTube: se muestra tu biblioteca y lo último que se pudo cargar." }
                }
            }
            if !online {
                div { class: "home-banner",
                    span { "Las recomendaciones en línea están desactivadas." }
                    button {
                        class: "btn-ghost",
                        onclick: move |_| {
                            enable.edit_settings(|settings| settings.discovery.online = true);
                            enable.refresh_home(true);
                        },
                        "Activar"
                    }
                }
            }
            if !featured.is_empty() {
                section { class: "browse-shelf", aria_label: "Sugerencias destacadas para ti",
                    div { class: "browse-shelf__header",
                        div { class: "browse-shelf__titles",
                            h2 { class: "browse-shelf__title", "Sugerencias destacadas para ti" }
                        }
                    }
                    div { class: "browse-shelf__scroller browse-shelf__scroller--featured",
                        for card in featured {
                            FeaturedCard { key: "{featured_key(&card)}", card }
                        }
                    }
                }
            }
            if let Some(track) = current {
                NowPlayingHero { track }
            }
            if recent.is_some() || favorites.is_some() {
                crate::cards::Shelf { title: "Escuchado recientemente",
                    if let Some(favorites) = favorites {
                        CoverCard {
                            title: "Canciones favoritas",
                            subtitle: artists_caption(&favorites.tracks),
                            generated: Some("collection:favorites".to_owned()),
                            icon: "heart",
                            onopen: move |_| open_favorites.navigate(Page::Favorites),
                        }
                    }
                    if let Some(recent) = recent {
                        for (index, track) in recent.tracks.iter().cloned().enumerate() {
                            SongCard { key: "{track.id}", track, context: recent.tracks.clone(), index }
                        }
                    }
                }
            }
            if !stations.is_empty() {
                crate::cards::Shelf { title: "Estaciones para ti", subtitle: "Música sin fin a partir de lo que te gusta",
                    for station in stations.iter().cloned() {
                        StationCard { key: "{station.key}", station }
                    }
                }
            }
            if !collections.is_empty() {
                crate::cards::Shelf { title: "Hechos para ti", subtitle: "Se actualizan con lo que escuchas",
                    for shelf in collections {
                        CollectionCard { key: "{shelf.key}", shelf }
                    }
                }
            }
            if !moods.is_empty() {
                crate::cards::Shelf { title: "Estados de ánimo", subtitle: "Medidos del sonido de tus canciones",
                    for shelf in moods {
                        CollectionCard { key: "{shelf.key}", shelf }
                    }
                }
            }
            if !replays.is_empty() {
                crate::cards::Shelf { title: "Replay", subtitle: "Lo que más has escuchado",
                    for shelf in replays {
                        CollectionCard { key: "{shelf.key}", shelf }
                    }
                }
            }
            for shelf in song_shelves {
                SongShelf { key: "{shelf.key}", shelf }
            }
            if loading && shelves.is_empty() {
                ShelfSkeleton {}
            }
            if !loading && shelves.is_empty() {
                EmptyState {
                    title: if library_empty { "Empieza a escuchar" } else { "Tu inicio está listo para aprender" },
                    hint: "Busca cualquier canción, artista o enlace de YouTube. Lo que escuches dará forma a tus recomendaciones.",
                    button {
                        class: "btn-accent",
                        onclick: move |_| search.navigate(Page::Search),
                        Icon { name: "search", size: 14 }
                        "Buscar música"
                    }
                }
            }
        }
    }
}

fn featured_key(card: &Featured) -> String {
    match card {
        Featured::Station(station) => station.key.clone(),
        Featured::Collection(shelf) => shelf.key.clone(),
    }
}

/// A large featured card: eyebrow, animated cover and a caption band.
#[component]
fn FeaturedCard(card: Featured) -> Element {
    let state = use_context::<AppState>();
    let (eyebrow, cover_key, title, caption, icon, faces) = match &card {
        Featured::Station(station) => (
            if station.kind == StationKind::Artist {
                format!(
                    "Destacando a {}",
                    station.title.trim_start_matches("Estación de ")
                )
            } else {
                "Hecha para ti".to_owned()
            },
            station.key.clone(),
            station.title.clone(),
            station.subtitle.clone(),
            if station.kind == StationKind::Artist {
                "mic"
            } else {
                "sparkles"
            },
            if station.kind == StationKind::Artist {
                station
                    .seeds
                    .iter()
                    .filter(|track| !track.artwork_url.is_empty())
                    .take(SPOTLIGHT_FACES)
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            },
        ),
        Featured::Collection(shelf) => (
            shelf.kind.eyebrow().to_owned(),
            shelf.key.clone(),
            shelf.title.clone(),
            artists_caption(&shelf.tracks),
            shelf.kind.icon(),
            Vec::new(),
        ),
    };
    let open = card.clone();
    rsx! {
        div { class: "featured-card",
            span { class: "featured-card__eyebrow", "{eyebrow}" }
            button {
                class: "featured-card__body",
                title: "{title}",
                style: "{crate::cards::cover_style(&cover_key)}",
                onclick: move |_| match &open {
                    Featured::Station(station) => state.play_station(station.clone()),
                    Featured::Collection(shelf) => state.open_detail(Detail::Collection(shelf.key.clone())),
                },
                div { class: "featured-card__art",
                    if faces.is_empty() {
                        GeneratedCover { cover: cover_key.clone(), title: title.clone(), icon }
                    } else {
                        div { class: "featured-card__faces",
                            for (index, face) in faces.iter().enumerate() {
                                div { key: "{face.id}", class: "featured-card__face featured-card__face--{index}",
                                    Thumb { src: artwork_src(face), size: "square" }
                                }
                            }
                        }
                    }
                    span { class: "featured-card__play", aria_hidden: "true",
                        Icon { name: "play", size: 15, fill: "currentColor", style: "margin-left:2px" }
                    }
                }
                div { class: "featured-card__caption",
                    span { class: "featured-card__title", "{title}" }
                    if !caption.is_empty() {
                        span { class: "featured-card__sub", "{caption}" }
                    }
                }
            }
        }
    }
}

/// A made-for-you collection in «Hechos para ti».
#[component]
fn CollectionCard(shelf: Shelf) -> Element {
    let state = use_context::<AppState>();
    let open = state.clone();
    let play = state.clone();
    let key = shelf.key.clone();
    let tracks = shelf.tracks.clone();
    rsx! {
        CoverCard {
            title: shelf.title.clone(),
            subtitle: artists_caption(&shelf.tracks),
            generated: Some(shelf.key.clone()),
            icon: shelf.kind.icon(),
            onopen: move |_| open.open_detail(Detail::Collection(key.clone())),
            onplay: move |_| play.play_all(tracks.clone(), false),
        }
    }
}

#[component]
pub(crate) fn SongShelf(shelf: Shelf) -> Element {
    let state = use_context::<AppState>();
    let everything = shelf.tracks.clone();
    let play = state.clone();
    rsx! {
        crate::cards::Shelf {
            title: shelf.title.clone(),
            subtitle: shelf.subtitle.clone(),
            action: Some(("Reproducir".to_owned(), EventHandler::new(move |_| play.play_all(everything.clone(), false)))),
            for (index, track) in shelf.tracks.iter().cloned().enumerate() {
                SongCard { key: "{track.id}", track, context: shelf.tracks.clone(), index }
            }
        }
    }
}

/// One song; playing it queues the rest of its shelf after it.
#[component]
fn SongCard(track: Track, context: Vec<Track>, index: usize) -> Element {
    let state = use_context::<AppState>();
    let active = state
        .current
        .read()
        .as_ref()
        .is_some_and(|current| current.id == track.id);
    let playing = active && *state.playing.read();
    let online = crate::remote::is_remote(&track);
    let play_state = state.clone();
    let menu_state = state.clone();
    let menu_track = track.clone();
    rsx! {
        div { class: if active { "shelf-card shelf-card--active" } else { "shelf-card" },
            button {
                class: "shelf-card__open",
                title: "{track.title}",
                oncontextmenu: move |event| crate::context_menu::show(event, crate::context_menu::home_items(menu_state.clone(), menu_track.clone())),
                onclick: move |_| {
                    if active {
                        play_state.toggle_play();
                    } else {
                        play_state.play_all(context[index..].to_vec(), false);
                    }
                },
                div { class: "shelf-card__cover",
                    Thumb { src: artwork_src(&track), size: "square", video: track.media_type == "video" }
                    if online {
                        span { class: "shelf-card__badge", Icon { name: "radio", size: 10 } "En línea" }
                    }
                    span { class: "shelf-card__play", aria_hidden: "true",
                        if playing {
                            Equalizer {}
                        } else {
                            Icon { name: "play", size: 14, fill: "currentColor", style: "margin-left:2px" }
                        }
                    }
                }
                span { class: "shelf-card__title", "{track.title}" }
                span { class: "shelf-card__sub", if track.artist.is_empty() { "—" } else { "{track.artist}" } }
            }
            FavoriteButton { track: track.clone(), size: 14 }
        }
    }
}

#[component]
fn NowPlayingHero(track: Track) -> Element {
    let state = use_context::<AppState>();
    let playing = *state.playing.read();
    let toggle = state.clone();
    let open = state.clone();
    let station = state.clone();
    let seed = track.clone();
    rsx! {
        section { class: "home-hero", aria_label: "Reproduciendo ahora",
            div { class: "home-hero__art",
                Thumb { src: artwork_src(&track), size: "square", video: track.media_type == "video" }
            }
            div { class: "home-hero__meta",
                span { class: "home-hero__eyebrow", if playing { "Reproduciendo" } else { "En pausa" } }
                span { class: "home-hero__title", title: "{track.title}", "{track.title}" }
                span { class: "home-hero__artist", "{track.artist}" }
                div { class: "home-hero__actions",
                    button {
                        class: "btn-accent",
                        onclick: move |_| toggle.toggle_play(),
                        Icon { name: if playing { "pause" } else { "play" }, size: 13, fill: "currentColor" }
                        if playing { "Pausar" } else { "Continuar" }
                    }
                    button {
                        class: "btn-secondary",
                        title: "Pantalla completa",
                        onclick: move |_| open.open_now_playing(),
                        Icon { name: "maximize", size: 13 }
                        "Pantalla completa"
                    }
                    button {
                        class: "btn-secondary",
                        title: "Estación a partir de esta canción",
                        onclick: move |_| station.start_song_station(&seed),
                        Icon { name: "radio", size: 13 }
                        "Crear estación"
                    }
                    FavoriteButton { track: track.clone(), size: 16 }
                }
            }
        }
    }
}

#[component]
fn ShelfSkeleton() -> Element {
    rsx! {
        div { class: "shelf", aria_hidden: "true",
            div { class: "shelf-skeleton",
                for index in 0..SHELF_ITEMS.min(6) {
                    div { key: "{index}", class: "shelf-skeleton__card" }
                }
            }
        }
    }
}

/// A Home collection opened as a page: generated cover, play actions and
/// the full track list.
#[component]
pub fn CollectionPage(collection: String) -> Element {
    let state = use_context::<AppState>();
    let shelf = state
        .discovery
        .shelves
        .read()
        .iter()
        .find(|shelf| shelf.key == collection)
        .cloned();
    let back = state.clone();
    let Some(shelf) = shelf else {
        return rsx! {
            div { class: "page-shell detail-page",
                crate::cards::DetailHero {
                    title: "Colección no disponible",
                    generated: Some(collection.clone()),
                    onback: move |_| back.go_back(),
                }
            }
        };
    };
    let play = state.clone();
    let shuffle = state.clone();
    let all = shelf.tracks.clone();
    let shuffled = shelf.tracks.clone();
    let count = shelf.tracks.len();
    rsx! {
        div { class: "page-shell detail-page",
            crate::cards::DetailHero {
                title: shelf.title.clone(),
                eyebrow: "Hecho para ti",
                facts: vec![shelf.subtitle.clone(), if count == 1 { "1 canción".to_owned() } else { format!("{count} canciones") }],
                generated: Some(shelf.key.clone()),
                onback: move |_| back.go_back(),
                button { class: "btn-accent", onclick: move |_| play.play_all(all.clone(), false),
                    Icon { name: "play", size: 13, fill: "currentColor" } "Reproducir"
                }
                button { class: "btn-secondary", title: "Aleatorio", onclick: move |_| shuffle.play_all(shuffled.clone(), true),
                    Icon { name: "shuffle", size: 13 }
                }
            }
            for (index, track) in shelf.tracks.into_iter().enumerate() {
                crate::app::TrackRow { key: "{track.id}", track, index }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        discovery::ShelfKind,
        test_support::{remote_track, render},
    };

    fn home() -> Element {
        rsx! { HomePage {} }
    }

    #[test]
    fn home_is_organized_like_a_music_app_with_featured_stations_and_collections() {
        let html = render(home, |state| {
            let tracks = vec![
                remote_track("aqz-KE-bpKQ", "La Bicicleta"),
                remote_track("abcdefghijk", "Favorita"),
            ];
            state.discovery.stations.set(vec![Station {
                key: "station:personal".into(),
                kind: StationKind::Personal,
                title: "Tu estación".into(),
                subtitle: "Lo que te gusta".into(),
                seeds: tracks.clone(),
            }]);
            state.discovery.shelves.set(vec![
                Shelf {
                    kind: ShelfKind::ContinueListening,
                    key: "continue".into(),
                    title: "Seguir escuchando".into(),
                    subtitle: String::new(),
                    tracks: vec![tracks[0].clone()],
                },
                Shelf {
                    kind: ShelfKind::Mix,
                    key: "mix".into(),
                    title: "Tu mezcla".into(),
                    subtitle: String::new(),
                    tracks: tracks.clone(),
                },
                Shelf {
                    kind: ShelfKind::BecauseYouListened,
                    key: "because:a".into(),
                    title: "Porque escuchaste «Bailando»".into(),
                    subtitle: "Enrique Iglesias".into(),
                    tracks: vec![tracks[0].clone()],
                },
                Shelf {
                    kind: ShelfKind::Favorites,
                    key: "favorites".into(),
                    title: "Tus favoritos".into(),
                    subtitle: String::new(),
                    tracks: vec![tracks[1].clone()],
                },
            ]);
        });
        let order = [
            "Sugerencias destacadas para ti",
            "Escuchado recientemente",
            "Estaciones para ti",
            "Hechos para ti",
            "Porque escuchaste",
        ];
        let positions: Vec<usize> = order
            .iter()
            .map(|text| html.find(text).unwrap_or_else(|| panic!("missing {text}")))
            .collect();
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "{positions:?}"
        );
        assert!(html.contains("Canciones favoritas"));
        assert!(html.contains("Tu estación"));
        assert!(html.contains("La Bicicleta"));
        assert!(html.contains("En línea"));
        assert!(html.contains("Channel"));
        assert!(html.contains("generated-cover"));
        assert!(html.contains("Actualizar recomendaciones"));
    }

    #[test]
    fn offline_and_local_only_states_explain_themselves() {
        let offline = render(home, |state| state.discovery.offline.set(true));
        assert!(offline.contains("Sin conexión con YouTube"));
        let local = render(home, |state| {
            state.edit_settings(|settings| settings.discovery.online = false)
        });
        assert!(local.contains("Las recomendaciones en línea están desactivadas"));
    }

    #[test]
    fn captions_list_distinct_artists_then_and_more() {
        let mut tracks: Vec<Track> = ["A", "B", "A", "C", "D"]
            .iter()
            .map(|artist| Track {
                artist: (*artist).into(),
                ..remote_track("aqz-KE-bpKQ", "x")
            })
            .collect();
        assert_eq!(artists_caption(&tracks), "A, B, C y más");
        tracks.truncate(2);
        assert_eq!(artists_caption(&tracks), "A, B");
    }
}
