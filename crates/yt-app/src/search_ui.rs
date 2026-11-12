use crate::{
    app::{Equalizer, Thumb},
    context_menu::Entry,
    icons::Icon,
    state::{time_label, AppState, Page},
};
use chrono::Datelike;
use dioxus::prelude::*;
use yt_media::{CancellationToken, CatalogSearch, SearchResult};

/// Genre shortcuts: chip label and the search they run. Home uses the first
/// one to start a profile that has no listening history yet.
pub(crate) fn genre_queries() -> [(&'static str, String); 8] {
    let year = chrono::Local::now().year();
    [
        ("Tendencias", format!("trending music {year}")),
        ("Pop", format!("pop hits {year}")),
        ("Reggaeton", format!("reggaeton {year} mix")),
        ("Rock", "classic rock greatest hits".into()),
        ("Lo-Fi", "lofi hip hop beats".into()),
        ("Electrónica", "electronic dance music".into()),
        ("Jazz", "jazz classics playlist".into()),
        ("K-Pop", format!("kpop hits {year}")),
    ]
}

/// Pause after the last keystroke before searching online.
const ONLINE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);
/// Shortest text searched online while typing (Enter always searches).
const MIN_LIVE_CHARS: usize = 2;
/// Library matches shown.
const LIBRARY_RESULTS: usize = 6;
/// Suggestions shown under the field.
const SUGGESTIONS: usize = 6;
/// Recent searches shown when the field is empty.
const RECENT_SEARCHES: usize = 10;
/// Pause after the last keystroke before asking the provider for
/// completions; shorter than the search debounce so they arrive first.
const SUGGEST_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(150);

#[component]
pub fn SearchPage() -> Element {
    let s = use_context::<AppState>();
    let mut query = use_signal(String::new);
    let mut focused = use_signal(|| false);
    let mut input = use_signal(|| None::<std::rc::Rc<MountedData>>);
    let page = s.page;
    use_effect(move || {
        if *page.read() == Page::Search {
            let element = input.read().clone();
            spawn(async move {
                if let Some(element) = element {
                    let _ = element.set_focus(true).await;
                }
            });
        }
    });
    // The library index is built off the UI thread, and only while this page
    // is shown; the previous index serves until the new one is ready.
    let library = s.library;
    let mut index = use_signal(|| None::<std::sync::Arc<crate::search::LibrarySearch>>);
    use_resource(move || async move {
        if *page.read() != Page::Search {
            return;
        }
        let tracks = library.read().clone();
        if let Ok(built) =
            tokio::task::spawn_blocking(move || crate::search::LibrarySearch::build(&tracks)).await
        {
            index.set(Some(std::sync::Arc::new(built)));
        }
    });
    let mut results = use_signal(|| None::<CatalogSearch>);
    // Provider completions and the text they complete.
    let mut provider = use_signal(|| (String::new(), Vec::<String>::new()));
    let mut suggesting = use_signal(|| None::<CancellationToken>);
    let suggest_state = s.clone();
    let mut fetch_suggestions = move |prefix: String| {
        if let Some(previous) = suggesting.write().take() {
            previous.cancel();
        }
        if prefix.trim().chars().count() < MIN_LIVE_CHARS {
            return;
        }
        let cancel = CancellationToken::new();
        suggesting.set(Some(cancel.clone()));
        let state = suggest_state.clone();
        spawn(async move {
            tokio::time::sleep(SUGGEST_DEBOUNCE).await;
            if cancel.is_cancelled() {
                return;
            }
            let found = state.provider_suggestions(&prefix, cancel.clone()).await;
            if !cancel.is_cancelled() {
                provider.set((prefix, found));
            }
        });
    };
    let mut playlist = use_signal(|| None::<(String, String, String, bool)>);
    let mut loading = use_signal(|| false);
    let mut generation = use_signal(|| 0u64);
    let mut pending = use_signal(|| None::<CancellationToken>);
    let search_state = s.clone();
    // Runs the online part of a search; `remember` records it in the history
    // (Enter, a suggestion or a category), typing alone does not. A newer
    // search cancels the request of the previous one.
    let run_search = move |term: String, remember: bool, delay: bool| {
        let term = term.trim().to_owned();
        let request = *generation.peek() + 1;
        generation.set(request);
        if let Some(previous) = pending.write().take() {
            previous.cancel();
        }
        if term.is_empty() {
            loading.set(false);
            results.set(None);
            playlist.set(None);
            return;
        }
        let state = search_state.clone();
        if remember {
            state.remember_search(&term);
        }
        let cancel = CancellationToken::new();
        pending.set(Some(cancel.clone()));
        spawn(async move {
            if delay {
                tokio::time::sleep(ONLINE_DEBOUNCE).await;
                if *generation.peek() != request {
                    return;
                }
            }
            loading.set(true);
            playlist.set(None);
            if url::Url::parse(&term)
                .is_ok_and(|url| url.query_pairs().any(|(key, _)| key == "list"))
            {
                let response = match state.services.youtube_client() {
                    Ok(youtube) => youtube
                        .playlist(&term)
                        .await
                        .map_err(|error| error.to_string()),
                    Err(error) => Err(error.to_string()),
                };
                if *generation.peek() != request {
                    return;
                }
                loading.set(false);
                match response {
                    Ok(value) => {
                        playlist.set(Some((
                            value.title,
                            value.thumbnail,
                            value.channel,
                            value.truncated,
                        )));
                        results.set(Some(CatalogSearch {
                            songs: value.items,
                            ..CatalogSearch::default()
                        }));
                    }
                    Err(error) => {
                        results.set(None);
                        state.notify_error(format!("No se pudo abrir la playlist: {error}"));
                    }
                }
                return;
            }
            let response = state.search_online(&term, cancel).await;
            if *generation.peek() != request {
                return;
            }
            loading.set(false);
            match response {
                Ok(found) => results.set(Some(found)),
                Err(error) => {
                    results.set(None);
                    state.notify_error(format!("No se pudo buscar: {error}"));
                }
            }
        });
    };
    let text = query.read().clone();
    let typing = !text.trim().is_empty();
    let searchable = index.read().clone();
    let ranked = searchable
        .as_ref()
        .map(|library| library.search(&text, LIBRARY_RESULTS, |track| s.track_affinity(track)))
        .unwrap_or_default();
    let suggestions = match &searchable {
        Some(library) if typing && *focused.read() => {
            let completions = provider.read();
            let online: &[String] =
                if !completions.0.is_empty() && text.trim().starts_with(completions.0.as_str()) {
                    &completions.1
                } else {
                    &[]
                };
            s.search_suggestions(&text, library, online, SUGGESTIONS)
        }
        _ => Vec::new(),
    };
    let history = s
        .search_history
        .read()
        .iter()
        .take(RECENT_SEARCHES)
        .cloned()
        .collect::<Vec<_>>();
    let found = results.read().clone();
    let collection = playlist.read().clone();
    let top = searchable
        .as_ref()
        .and_then(|library| crate::search::top_result(&text, library, &ranked, found.as_ref()));
    let library_hits: Vec<_> = ranked.into_iter().map(|(track, _)| track).collect();
    let mut submit = run_search.clone();
    let mut live = run_search.clone();
    let mut clear = run_search.clone();
    let clear_history = s.clone();
    rsx! { div { class: "page-shell search-page",
        style { {include_str!("search_ui.css")} }
        div { class: "page-header", h1 { class: "page-title", "Buscar" } }
        form {
            class: "search-field",
            onsubmit: move |event| {
                event.prevent_default();
                focused.set(false);
                submit(query.peek().clone(), true, false);
            },
            div { class: "input-search-wrap",
                span { class: "input-search-icon", Icon { name: "search", size: 16 } }
                input {
                    id: "ytu-search",
                    class: "input-search input-search--hero",
                    placeholder: "Artistas, canciones, álbumes o enlaces de YouTube",
                    aria_label: "Buscar música",
                    autocomplete: "off",
                    value: "{query}",
                    autofocus: true,
                    onmounted: move |event| input.set(Some(event.data())),
                    onfocus: move |_| focused.set(true),
                    onblur: move |_| {
                        // Let a click on a suggestion land before the list closes.
                        spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                            focused.set(false);
                        });
                    },
                    oninput: move |event| {
                        let value = event.value();
                        query.set(value.clone());
                        fetch_suggestions(value.trim().to_owned());
                        if value.trim().chars().count() >= MIN_LIVE_CHARS {
                            live(value, false, true);
                        } else {
                            live(String::new(), false, false);
                        }
                    },
                }
                if typing {
                    button {
                        r#type: "button",
                        class: "input-search-clear",
                        aria_label: "Limpiar",
                        onclick: move |_| {
                            query.set(String::new());
                            clear(String::new(), false, false);
                            let element = input.peek().clone();
                            spawn(async move {
                                if let Some(element) = element {
                                    let _ = element.set_focus(true).await;
                                }
                            });
                        },
                        "×"
                    }
                }
            }
            if !suggestions.is_empty() {
                ul { class: "search-suggestions", role: "listbox", aria_label: "Sugerencias",
                    for suggestion in suggestions {
                        li { key: "{suggestion.text}",
                            button {
                                r#type: "button",
                                class: "search-suggestion",
                                role: "option",
                                onmousedown: |event| event.prevent_default(),
                                onclick: {
                                    let mut run = run_search.clone();
                                    let text = suggestion.text.clone();
                                    move |_| {
                                        query.set(text.clone());
                                        focused.set(false);
                                        run(text.clone(), true, false);
                                    }
                                },
                                Icon { name: suggestion.icon, size: 14 }
                                span { "{suggestion.text}" }
                            }
                        }
                    }
                }
            }
        }
        if !typing {
            if !history.is_empty() {
                section { class: "search-recent",
                    div { class: "browse-shelf__header",
                        h2 { class: "browse-shelf__title", "Búsquedas recientes" }
                        button { class: "section-action", onclick: move |_| clear_history.clear_search_history(), "Borrar" }
                    }
                    div { class: "chip-row",
                        for entry in history {
                            span { key: "{entry.query}", class: "chip search-recent__chip",
                                button {
                                    class: "search-recent__text",
                                    onclick: {
                                        let mut run = run_search.clone();
                                        let text = entry.query.clone();
                                        move |_| {
                                            query.set(text.clone());
                                            run(text.clone(), true, false);
                                        }
                                    },
                                    Icon { name: "history", size: 12 }
                                    "{entry.query}"
                                }
                                button {
                                    class: "search-recent__remove",
                                    title: "Quitar del historial",
                                    aria_label: "Quitar {entry.query} del historial",
                                    onclick: {
                                        let state = s.clone();
                                        let text = entry.query.clone();
                                        move |_| state.forget_search(&text)
                                    },
                                    "×"
                                }
                            }
                        }
                    }
                }
            }
            h2 { class: "detail-section-title", "Explorar categorías" }
            div { class: "search-categories",
                for (label, term) in genre_queries() {
                    button {
                        key: "{label}",
                        class: "search-category",
                        onclick: {
                            let mut run = run_search.clone();
                            move |_| {
                                query.set(label.to_owned());
                                run(term.clone(), false, false);
                            }
                        },
                        crate::cards::GeneratedCover { cover: format!("genre:{label}"), title: label.to_owned(), icon: "music" }
                    }
                }
            }
        } else {
            if let Some((title, thumbnail, channel, truncated)) = collection {
                div { class: "anim-fade",
                    div { class: "collection-hero",
                        Thumb { src: thumbnail, size: "xl", icon_size: Some(30) }
                        div { class: "collection-hero__meta",
                            span { class: "collection-hero__eyebrow", "Playlist de YouTube" }
                            span { class: "collection-hero__title", "{title}" }
                            span { class: "collection-hero__sub", if !channel.is_empty() { "{channel} · " } "{found.as_ref().map_or(0, |found| found.songs.len())} vídeos" }
                            div { class: "collection-hero__actions",
                                for (label, video) in [("Descargar todo en MP3", false), ("MP4", true)] {
                                    button {
                                        class: if video { "btn-secondary" } else { "btn-accent" },
                                        onclick: {
                                            let state = s.clone();
                                            let items = found.as_ref().map(|found| found.songs.clone()).unwrap_or_default();
                                            let name = title.clone();
                                            move |_| state.download_playlist(&name, items.clone(), video)
                                        },
                                        if !video { Icon { name: "folder-down", size: 14 } }
                                        "{label}"
                                    }
                                }
                            }
                        }
                    }
                    if truncated { p { class: "rust-settings-note", "Se alcanzó el límite de resultados. Se descargarán únicamente los vídeos mostrados." } }
                    for (index, item) in found.as_ref().map(|found| found.songs.clone()).unwrap_or_default().into_iter().enumerate() {
                        PlaylistSearchRow { key: "{index}-{item.id}", item, index }
                    }
                }
            } else {
                div { class: "search-results anim-fade",
                    if let Some(top) = top {
                        div { class: "search-top-row",
                            crate::search_results::TopResultCard { top }
                            if let Some(found) = found.as_ref() {
                                div { class: "search-top-songs",
                                    h2 { class: "browse-shelf__title", "Canciones" }
                                    for item in found.songs.iter().take(crate::search::TOP_SONGS).cloned() {
                                        SearchRow { key: "{item.id}", item }
                                    }
                                }
                            }
                        }
                    }
                    if !library_hits.is_empty() {
                        h2 { class: "detail-section-title", "En tu biblioteca" }
                        for (index, track) in library_hits.into_iter().enumerate() {
                            crate::app::TrackRow { key: "{track.id}", track, index }
                        }
                    }
                    if let Some(found) = found {
                        crate::search_results::OnlineSections {
                            found,
                            onplaylist: {
                                let mut run = run_search.clone();
                                move |id: String| {
                                    let link = format!("https://www.youtube.com/playlist?list={id}");
                                    query.set(link.clone());
                                    run(link, true, false);
                                }
                            },
                        }
                    } else if *loading.read() {
                        p { class: "rust-loading", role: "status", "Buscando en YouTube…" }
                    }
                }
            }
        }
    } }
}

/// One online song or video: artwork (or its number in an album), title,
/// artist and play count, with play, queue, favorite and download actions.
#[component]
pub(crate) fn SearchRow(item: SearchResult, #[props(default)] index: Option<usize>) -> Element {
    let state = use_context::<AppState>();
    let active = state
        .current
        .read()
        .as_ref()
        .is_some_and(|track| track.id == format!("{}{}", yt_core::REMOTE_ID_PREFIX, item.id));
    let playing = active && *state.playing.read();
    let loading = active && *state.stream_loading.read();
    let row_state = state.clone();
    let row_item = item.clone();
    let play_state = state.clone();
    let play_item = item.clone();
    let next_state = state.clone();
    let next_item = item.clone();
    let menu_state = state.clone();
    let menu_item = item.clone();
    let artist = yt_core::clean_artist(&item.channel);
    rsx! {
        div {
            class: if active { "row row--active" } else { "row" },
            title: "Reproducir sin descargar",
            onclick: move |_| row_state.play_remote(row_item.clone(), false),
            oncontextmenu: move |event| crate::context_menu::show(event, search_items(menu_state.clone(), menu_item.clone())),
            if let Some(index) = index {
                span { class: "row__index",
                    if playing {
                        Equalizer {}
                    } else {
                        "{index + 1}"
                    }
                }
            } else {
                Thumb { src: item.thumbnail.clone(), size: "lg",
                    if item.duration_seconds > 0 {
                        span { class: "thumb-duration", "{time_label(item.duration_seconds as f64)}" }
                    }
                    if playing {
                        div { class: "thumb-playing", Equalizer {} }
                    }
                }
            }
            div { class: "row__meta",
                div { class: if active { "row__title row__title--active" } else { "row__title" }, "{item.title}" }
                div { class: "row__sub",
                    "{artist}"
                    if item.view_count > 0 {
                        " · {crate::search_results::views_label(item.view_count)} reproducciones"
                    }
                }
            }
            if index.is_some() && item.duration_seconds > 0 {
                span { class: "row__detail", "{time_label(item.duration_seconds as f64)}" }
            }
            div { class: "row__actions",
                button {
                    class: "btn-icon",
                    title: if playing { "Pausar" } else { "Reproducir (stream)" },
                    disabled: loading,
                    onclick: move |event| {
                        event.stop_propagation();
                        play_state.play_remote(play_item.clone(), false);
                    },
                    Icon {
                        name: if loading { "loading" } else if playing { "pause" } else { "play" },
                        size: 14,
                        fill: if loading { "none" } else { "currentColor" },
                    }
                }
                button {
                    class: "btn-icon",
                    title: "Reproducir a continuación",
                    onclick: move |event| {
                        event.stop_propagation();
                        next_state.queue_remote(next_item.clone());
                    },
                    Icon { name: "list-plus", size: 14 }
                }
                crate::favorites_ui::FavoriteButton { track: crate::remote::track_from_result(item.clone(), false), size: 14 }
                DownloadActions { item }
            }
        }
    }
}
#[component]
fn PlaylistSearchRow(item: SearchResult, index: usize) -> Element {
    rsx! {div{class:"row",span{class:"row__index","{index+1}"}Thumb{src:item.thumbnail.clone(),size:"sm"}div{class:"row__meta",div{class:"row__title","{item.title}"}div{class:"row__sub","{item.channel}"}}if item.duration_seconds>0{span{class:"row__detail","{time_label(item.duration_seconds as f64)}"}}div{class:"row__actions",DownloadActions{item}}}}
}
#[component]
fn DownloadActions(item: SearchResult) -> Element {
    let state = use_context::<AppState>();
    rsx! {for(label,video)in[("Descargar MP3",false),("Descargar vídeo",true)]{button{class:"btn-icon",title:label,onclick:{let state=state.clone();let item=item.clone();move|event|{event.stop_propagation();state.download(item.clone(),video,false);}},Icon{name:if video{"video"}else{"music"},size:14}}}}
}
fn search_items(state: AppState, item: SearchResult) -> Vec<Entry> {
    let play = state.clone();
    let play_item = item.clone();
    let next = state.clone();
    let next_item = item.clone();
    let mut entries = vec![
        Entry::action("Reproducir", move || {
            play.play_remote(play_item.clone(), false)
        }),
        Entry::action("Reproducir a continuación", move || {
            next.queue_remote(next_item.clone())
        }),
        crate::favorites::favorite_entry(
            &state,
            &crate::remote::track_from_result(item.clone(), false),
        ),
        Entry::separator(),
    ];
    entries.extend(crate::navigation::browse_entries(
        &state,
        &crate::remote::track_from_result(item.clone(), false),
    ));
    entries.push(Entry::separator());
    for (label, video) in [("Descargar como MP3", false), ("Descargar como MP4", true)] {
        let state = state.clone();
        let item = item.clone();
        entries.push(Entry::action(label, move || {
            state.download(item.clone(), video, false)
        }));
    }
    let copy = state.clone();
    let link = item.url.clone();
    let open = state.clone();
    let url = item.url;
    entries.extend([
        Entry::separator(),
        Entry::action("Copiar enlace", move || {
            crate::native::copy_text(&copy, &link)
        }),
        Entry::action("Abrir en YouTube", move || {
            crate::native::open_url(&open, &url)
        }),
    ]);
    entries
}
