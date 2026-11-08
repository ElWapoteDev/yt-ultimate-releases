use crate::{
    app::{EmptyState, MediaCard, Thumb, TrackRow},
    icons::Icon,
    state::*,
};
use chrono::{Datelike, Timelike};
use dioxus::prelude::*;
use yt_core::Track;
use yt_media::SearchResult;

#[component]
pub fn HomePage() -> Element {
    let mut s = use_context::<AppState>();
    let hour = chrono::Local::now().hour();
    let greeting = match hour {
        6..=11 => "Buenos días",
        12..=19 => "Buenas tardes",
        _ => "Buenas noches",
    };
    let recent = s
        .library
        .read()
        .iter()
        .take(12)
        .cloned()
        .collect::<Vec<_>>();
    let history = s.history.read().iter().take(6).cloned().collect::<Vec<_>>();
    let mut library_nav = s.clone();
    rsx! { div { class: "page-shell",
        div { class: "home-hero", h1 { class: "home-hero__greeting", "{greeting}" } div { class: "home-hero__sub", "Tu música, sin distracciones." } }
        if recent.is_empty() {
            EmptyState { title: "Empieza tu biblioteca", hint: "Busca cualquier canción, artista o enlace de YouTube y descárgala para escucharla aquí, incluso sin conexión.",
                button { class: "btn-accent", onclick: move |_| s.page.set(Page::Search), Icon { name: "search", size: 14 } "Buscar música" }
            }
        }
        if !history.is_empty() {
            div { class: "section", div { class: "section-header", h2 { class: "section-title", "Escuchado recientemente" } }
                div { class: "media-grid", for track in history { MediaCard { key: "{track.id}", track } } }
            }
        }
        if !recent.is_empty() {
            div { class: "section", div { class: "section-header", h2 { class: "section-title", "Añadido recientemente" }
                button { class: "section-action", onclick: move |_| library_nav.page.set(Page::Library), "Ver todo" Icon { name: "arrow", size: 13 } }
            }
                div { class: "media-grid", for track in recent { MediaCard { key: "{track.id}", track } } }
            }
        }
    } }
}

#[component]
pub fn LibraryPage() -> Element {
    let s = use_context::<AppState>();
    let mut query = use_signal(String::new);
    let mut filter = use_signal(|| "all");
    let mut grid = use_signal(|| true);
    let mut favorite = use_signal(|| false);
    let q = query.read().to_lowercase();
    let tracks = s
        .library
        .read()
        .iter()
        .filter(|t| {
            (*filter.read() == "all" || t.media_type == *filter.read())
                && (!*favorite.read() || t.favorite)
                && format!("{} {} {}", t.title, t.artist, t.album)
                    .to_lowercase()
                    .contains(&q)
        })
        .cloned()
        .collect::<Vec<_>>();
    let play_all = tracks.clone();
    let shuffled = tracks.clone();
    let play_state = s.clone();
    let shuffle_state = s.clone();
    let import_state = s.clone();
    let folder_state = s.clone();
    let empty_import_state = s.clone();
    rsx! { div { class: "page-shell",
        div { class: "toolbar",
            div { class: "toolbar__left", h1 { class: "page-title", "Canciones" } span { class: "page-count", "{tracks.len()}" } }
            div { class: "toolbar__right",
                if !tracks.is_empty() {
                    button { class: "btn-accent", onclick: move |_| play_state.play_all(play_all.clone(), false), Icon { name: "play", size: 13 } "Reproducir" }
                    button { class: "btn-secondary", title: "Reproducción aleatoria", onclick: move |_| shuffle_state.play_all(shuffled.clone(), true), Icon { name: "shuffle", size: 13 } }
                }
                button { class: "btn-secondary", disabled: *s.busy.read(), onclick: move |_| import_state.import_files(false), Icon { name: "plus", size: 13 } "Importar" }
                button { class: "btn-icon", title: "Importar carpeta", disabled: *s.busy.read(), onclick: move |_| folder_state.import_files(true), Icon { name: "folder" } }
                div { class: "segmented", for (id, label) in [("all", "Todo"), ("audio", "Audio"), ("video", "Vídeo")] {
                    button { class: if *filter.read() == id { "segment active" } else { "segment" }, onclick: move |_| filter.set(id), "{label}" }
                } }
                button { class: "btn-icon", title: "Favoritos", aria_pressed: *favorite.read(), style: if *favorite.read() { "color:var(--accent)" } else { "" }, onclick: move |_| favorite.toggle(), Icon { name: "heart" } }
                button { class: "btn-icon", title: "Cambiar vista", onclick: move |_| grid.toggle(), Icon { name: if *grid.read() { "list" } else { "grid" } } }
            }
        }
        if !s.library.read().is_empty() {
            div { class: "input-search-wrap", style: "max-width:360px;margin-bottom:22px",
                input { class: "input-search", style: "padding-left:12px", placeholder: "Filtrar canciones", aria_label: "Filtrar canciones", value: "{query}", oninput: move |e| query.set(e.value()) }
            }
        }
        if *s.busy.read() { p { class: "rust-loading", role: "status", "Procesando archivos…" } }
        if s.library.read().is_empty() {
            EmptyState { title: "Aún no hay música", hint: "Descarga desde Buscar o añade música que ya tengas en tu equipo.",
                button { class: "btn-accent", onclick: move |_| empty_import_state.import_files(false), Icon { name: "plus", size: 14 } "Importar música" }
            }
        } else if tracks.is_empty() {
            EmptyState { title: "Nada coincide", hint: "Prueba con otro término o cambia el filtro.", icon: "search" }
        } else if *grid.read() {
            div { class: "media-grid", for track in tracks { MediaCard { key: "{track.id}", track } } }
        } else {
            for (index, track) in tracks.into_iter().enumerate() { TrackRow { key: "{track.id}", track, index } }
        }
    } }
}

#[component]
pub fn SearchPage() -> Element {
    let s = use_context::<AppState>();
    let mut query = use_signal(String::new);
    let mut results = use_signal(Vec::<SearchResult>::new);
    let mut playlist = use_signal(|| None::<(String, String, bool)>);
    let mut loading = use_signal(|| false);
    let mut searched = use_signal(|| false);
    let mut generation = use_signal(|| 0u64);
    let search_state = s.clone();
    let run_search = move |term: String| {
        let term = term.trim().to_owned();
        if term.is_empty() {
            return;
        }
        let request = *generation.peek() + 1;
        generation.set(request);
        loading.set(true);
        searched.set(true);
        playlist.set(None);
        let state = search_state.clone();
        spawn(async move {
            if url::Url::parse(&term)
                .is_ok_and(|url| url.query_pairs().any(|(key, _)| key == "list"))
            {
                let response = state.services.youtube.playlist(&term).await;
                if *generation.peek() != request {
                    return;
                }
                loading.set(false);
                match response {
                    Ok(value) => {
                        playlist.set(Some((value.title, value.thumbnail, value.truncated)));
                        results.set(value.items);
                    }
                    Err(error) => {
                        results.set(Vec::new());
                        state.notify(format!("No se pudo abrir la playlist: {error}"));
                    }
                }
                return;
            }
            let response = if term.starts_with("https://") || term.starts_with("http://") {
                state
                    .services
                    .youtube
                    .metadata(&term)
                    .await
                    .map(|item| vec![item])
            } else {
                state.services.youtube.search(&term, 25).await
            };
            if *generation.peek() != request {
                return;
            }
            loading.set(false);
            match response {
                Ok(items) => results.set(items),
                Err(e) => {
                    results.set(Vec::new());
                    state.notify(format!("No se pudo buscar: {e}"));
                }
            }
        });
    };
    let mut submit_search = run_search.clone();
    let year = chrono::Local::now().year();
    let categories = [
        ("Tendencias", format!("trending music {year}")),
        ("Pop", format!("pop hits {year}")),
        ("Reggaeton", format!("reggaeton {year}")),
        ("Rock", "classic rock greatest hits".into()),
        ("Lo-Fi", "lofi hip hop beats".into()),
        ("Electrónica", "electronic dance music".into()),
        ("Jazz", "jazz classics".into()),
        ("K-Pop", format!("kpop hits {year}")),
    ];
    let rows = results.read().clone();
    let collection = playlist.read().clone();
    rsx! { div { class: "page-shell",
        div { class: "page-header", h1 { class: "page-title", "Buscar" } }
        form { class: "rust-form", onsubmit: move |e| { e.prevent_default(); submit_search(query.read().clone()); },
            div { class: "input-search-wrap", input { class: "input-search", style: "padding-left:14px", placeholder: "Canción, artista o enlace de YouTube", aria_label: "Buscar música", value: "{query}", oninput: move |e| query.set(e.value()) } }
            button { class: "btn-accent", r#type: "submit", disabled: *loading.read(), Icon { name: "search", size: 14 } "Buscar" }
        }
        div { class: "chip-row", style: "margin-bottom:28px", for (label, term) in categories {
            button { class: "chip", onclick: { let mut search = run_search.clone(); move |_| { query.set(term.clone()); search(term.clone()); } }, "{label}" }
        } }
        if *loading.read() { p { class: "rust-loading", role: "status", "Buscando…" } }
        else if !rows.is_empty() {
            if let Some((title,thumbnail,truncated))=collection {
                div {class:"collection-hero",Thumb{src:thumbnail,size:"xl"}
                    div{class:"collection-hero__meta",span{class:"collection-hero__eyebrow","Playlist"}span{class:"collection-hero__title","{title}"}span{class:"collection-hero__sub","{rows.len()} vídeos disponibles"}
                        div {class:"collection-hero__actions",
                            for (label,video) in [("Descargar MP3",false),("Descargar vídeos",true)] {
                                button {class:"btn-accent",onclick:{let state=s.clone();let items=rows.clone();let name=title.clone();move |_|state.download_playlist(&name,items.clone(),video)},Icon{name:"download",size:14}"{label}"}
                            }
                        }
                    }
                }
                if truncated {p{class:"rust-settings-note","Se alcanzó el límite de resultados. Se descargarán únicamente los vídeos mostrados."}}
            }
            div { class: "section-header", h2 { class: "section-title", "Resultados" } span { class: "section-count", "{rows.len()}" } }
            for (index,item) in rows.into_iter().enumerate() { SearchRow { key: "{index}-{item.id}", item } }
        } else if *searched.read() { EmptyState { title: "Sin resultados", hint: "Intenta con otros términos o pega un enlace de YouTube.", icon: "search" } }
        else { EmptyState { title: "¿Qué quieres escuchar?", hint: "Escribe y pulsa Enter, pega un enlace de YouTube, o explora un género.", icon: "search" } }
    } }
}

#[component]
fn SearchRow(item: SearchResult) -> Element {
    let s = use_context::<AppState>();
    let audio = item.clone();
    let video = item.clone();
    let play = item.clone();
    let audio_state = s.clone();
    let video_state = s.clone();
    rsx! { div { class: "row",
        Thumb { src: item.thumbnail.clone(), size: "lg" }
        div { class: "row__meta", div { class: "row__title", "{item.title}" } div { class: "row__sub", "{item.channel}" } }
        span { class: "row__detail", "{time_label(item.duration_seconds as f64)}" }
        div { class: "row__actions",
            button { class: "btn-icon", title: "Descargar y reproducir", onclick: move |_| s.download(play.clone(), false, true), Icon { name: "play", size: 14 } }
            button { class: "btn-icon", title: "Descargar MP3", onclick: move |_| audio_state.download(audio.clone(), false, false), Icon { name: "music", size: 14 } }
            button { class: "btn-icon", title: "Descargar vídeo", onclick: move |_| video_state.download(video.clone(), true, false), Icon { name: "video", size: 14 } }
        }
    } }
}

#[component]
pub fn PlaylistsPage() -> Element {
    let s = use_context::<AppState>();
    let mut selected = use_signal(|| None::<String>);
    let mut creating = use_signal(|| false);
    let mut name = use_signal(String::new);
    let mut rename = use_signal(String::new);
    let playlists = s.playlists.read().clone();
    let selection = selected
        .read()
        .clone()
        .and_then(|id| playlists.iter().find(|p| p.id == id).cloned());
    // The library signal also invalidates membership after a successful edit.
    let _ = s.library.read();
    let tracks = selection
        .as_ref()
        .map(|p| s.services.core.playlist_tracks(&p.id).unwrap_or_default())
        .unwrap_or_default();
    let mut revision = use_signal(|| 0usize);
    let _ = revision.read();
    let create_state = s.clone();
    let rename_state = s.clone();
    let delete_state = s.clone();
    let rename_id = selection.as_ref().map(|p| p.id.clone());
    let delete_id = rename_id.clone();
    let play_state = s.clone();
    let play_tracks = tracks.clone();
    rsx! { div { class: "split-page",
        div { class: "split-page__top",
            div { class: "toolbar", div { class: "toolbar__left", h1 { class: "page-title", "Playlists" } span { class: "page-count", "{playlists.len()}" } }
                button { class: "btn-secondary", onclick: move |_| creating.toggle(), Icon { name: "plus", size: 13 } "Nueva" }
            }
            if *creating.read() {
                form { class: "rust-form", onsubmit: move |e| { e.prevent_default();
                    let playlist_name = name.read().clone();
                    match create_state.services.core.create_playlist(&playlist_name) { Ok(p) => { selected.set(Some(p.id)); name.set(String::new()); creating.set(false); create_state.refresh(); }, Err(e) => create_state.notify(e.to_string()) }
                },
                    input { class: "rust-inline-input", placeholder: "Nombre de la playlist…", aria_label: "Nombre de la playlist", value: "{name}", oninput: move |e| name.set(e.value()) }
                    button { class: "btn-accent", r#type: "submit", disabled: name.read().trim().is_empty(), "Crear" }
                }
            }
        }
        div { class: "split-page__body",
            aside { class: "playlist-rail",
                if playlists.is_empty() { EmptyState { title: "Sin playlists", hint: "Crea la primera con «Nueva».", icon: "playlist" } }
                for playlist in playlists {
                    button { class: if selection.as_ref().is_some_and(|p| p.id == playlist.id) { "row row--active" } else { "row" }, onclick: move |_| { selected.set(Some(playlist.id.clone())); rename.set(playlist.name.clone()); },
                        Thumb { size: "md" }
                        div { class: "row__meta", div { class: "row__title", "{playlist.name}" } div { class: "row__sub", "{playlist.track_count} canciones" } }
                    }
                }
            }
            div { class: "split-page__detail",
                if let Some(playlist) = selection {
                    div { class: "collection-hero", Thumb { size: "xl" }
                        div { class: "collection-hero__meta", span { class: "collection-hero__eyebrow", "Playlist" } span { class: "collection-hero__title", "{playlist.name}" } span { class: "collection-hero__sub", "{tracks.len()} canciones" }
                            div { class: "collection-hero__actions",
                                button { class: "btn-accent", disabled: tracks.is_empty(), onclick: move |_| play_state.play_all(play_tracks.clone(), false), Icon { name: "play", size: 13 } "Reproducir" }
                                button { class: "btn-icon rust-danger", title: "Eliminar playlist", onclick: move |_| {
                                    if let Some(id) = &delete_id { match delete_state.services.core.delete_playlist(id) { Ok(_) => { selected.set(None); delete_state.refresh(); }, Err(e) => delete_state.notify(e.to_string()) } }
                                }, Icon { name: "trash", size: 14 } }
                            }
                        }
                    }
                    form { class: "rust-form", onsubmit: move |e| { e.prevent_default(); if let Some(id) = &rename_id {
                        match rename_state.services.core.rename_playlist(id, &rename.read()) { Ok(_) => rename_state.refresh(), Err(e) => rename_state.notify(e.to_string()) }
                    } },
                        input { class: "rust-inline-input", value: "{rename}", placeholder: "Cambiar nombre", aria_label: "Cambiar nombre de playlist", oninput: move |e| rename.set(e.value()) }
                        button { class: "btn-secondary", r#type: "submit", disabled: rename.read().trim().is_empty(), "Renombrar" }
                    }
                    if tracks.is_empty() { EmptyState { title: "Playlist vacía", hint: "Añade canciones desde Canciones con clic derecho o el botón de opciones." } }
                    for (index, track) in tracks.iter().cloned().enumerate() {
                        PlaylistTrackRow { key: "{track.id}", playlist: playlist.id.clone(), track, index, changed: move |_| { revision += 1; } }
                    }
                } else { EmptyState { title: "Selecciona una playlist", hint: "Añade canciones desde Canciones con clic derecho → Añadir a playlist.", icon: "playlist" } }
            }
        }
    } }
}

#[component]
fn PlaylistTrackRow(
    playlist: String,
    track: Track,
    index: usize,
    changed: EventHandler<()>,
) -> Element {
    let s = use_context::<AppState>();
    let remove_id = track.id.clone();
    let remove_playlist = playlist.clone();
    let remove_state = s.clone();
    rsx! { div { style: "display:flex;align-items:center",
        div { style: "flex:1;min-width:0", TrackRow { track, index } }
        button { class: "btn-icon-sm", title: "Mover arriba", disabled: index == 0, onclick: move |_| {
            let result = s.services.core.playlist_tracks(&playlist).and_then(|tracks| {
                let mut ids = tracks.into_iter().map(|t| t.id).collect::<Vec<_>>();
                if index > 0 && index < ids.len() { ids.swap(index, index - 1); }
                s.services.core.reorder_playlist(&playlist, &ids)
            });
            match result { Ok(_) => { s.refresh(); changed.call(()); }, Err(e) => s.notify(e.to_string()) }
        }, "↑" }
        button { class: "btn-icon-sm", title: "Quitar de la playlist", onclick: move |_| {
            match remove_state.services.core.remove_from_playlist(&remove_playlist, &remove_id) { Ok(_) => { remove_state.refresh(); changed.call(()); }, Err(e) => remove_state.notify(e.to_string()) }
        }, Icon { name: "close", size: 13 } }
    } }
}

#[component]
pub fn DownloadsPage() -> Element {
    let s = use_context::<AppState>();
    let downloads = s.downloads.read().clone();
    rsx! { div { class: "page-shell", div { class: "toolbar", div { class: "toolbar__left", h1 { class: "page-title", "Descargas" } span { class: "page-count", "{downloads.len()}" } } }
        if downloads.is_empty() { EmptyState { title: "No hay descargas", hint: "Las descargas que inicies desde Buscar aparecerán aquí.", icon: "download" } }
        for download in downloads {
            div { class: "rust-download",
                div { class: "rust-download-head", span { "{download.title}" } span { class: "rust-status", "{download.status}" }
                    if matches!(download.status.as_str(), "Preparando" | "Descargando") {
                        button { class: "btn-icon-sm", title: "Cancelar descarga", onclick: { let state = s.clone(); let id = download.id.clone(); move |_| { if let Some(token) = state.cancellations.read().get(&id) { token.cancel(); } } }, Icon { name: "close" } }
                    }
                    if matches!(download.status.as_str(),"Error"|"Cancelado"|"Interrumpido") {
                        button { class:"btn-secondary",onclick:{let state=s.clone();let id=download.id.clone();move |_|state.retry_download(&id)},"Reintentar" }
                    }
                }
                progress { class: "rust-progress", max: "100", value: "{download.progress}" }
                if !download.detail.is_empty() { p { class: "rust-download-detail", "{download.detail}" } }
            }
        }
    } }
}

#[component]
pub fn SettingsPage() -> Element {
    let s = use_context::<AppState>();
    let settings = s.settings.read().clone();
    let copy_state = s.clone();
    let reference_state = s.clone();
    let advanced_state = s.clone();
    let scan_state = s.clone();
    let developer_state = s.clone();
    let total_size = s.library.read().iter().map(|t| t.size_bytes).sum::<u64>();
    let scan_action = s.clone();
    rsx! { div { class: "page-shell page-shell--narrow",
        div { class: "page-header", h1 { class: "page-title", "Ajustes" } }
        section { class: "settings-section",
            div { class: "settings-section__header", div { class: "settings-section__icon", Icon { name: "music", size: 19 } } div { div { class: "settings-section__title", "Calidad de audio" } div { class: "settings-section__sub", "Descargas y exportaciones MP3" } } }
            for (value, description) in [("128", "Ahorro de espacio"), ("192", "Equilibrado"), ("256", "Alta calidad"), ("320", "Máxima calidad")] {
                button { class: "settings-row", onclick: { let state = s.clone(); move |_| { let mut settings = state.settings.read().clone(); settings.audio_quality = value.into(); state.save_settings(settings); } },
                    span { class: "settings-row__label", span { class: "settings-row__title", "{value} kbps" } span { class: "settings-row__sub", "{description}" } }
                    span { class: if settings.audio_quality == value { "settings-row__check active" } else { "settings-row__check" }, Icon { name: "check", size: 11 } }
                }
            }
        }
        section { class: "settings-section",
            div { class: "settings-section__header", div { class: "settings-section__icon", Icon { name: "folder", size: 19 } }
                div { div { class: "settings-section__title", "Biblioteca" } div { class: "settings-section__sub", "Tu música y tus archivos" } }
            }
            div { class: "settings-stat-grid",
                div { class: "settings-stat", div { class: "settings-stat__value", "{s.library.read().len()}" } div { class: "settings-stat__label", "Canciones" } }
                div { class: "settings-stat", div { class: "settings-stat__value", "{s.playlists.read().len()}" } div { class: "settings-stat__label", "Playlists" } }
                div { class: "settings-stat", div { class: "settings-stat__value", "{size_label(total_size)}" } div { class: "settings-stat__label", "Almacenamiento" } }
            }
            p { class: "rust-settings-note", "{s.services.library_dir.display()}" }
            button { class: "settings-row", onclick: move |_| {
                let state = scan_action.clone(); spawn(async move {
                    let core = state.services.core.clone();
                    match tokio::task::spawn_blocking(move || core.scan_library()).await { Ok(Ok(_)) => { state.refresh(); state.notify("Biblioteca actualizada"); }, Ok(Err(e)) => state.notify(e.to_string()), Err(e) => state.notify(e.to_string()) }
                });
            }, span { class: "settings-row__title", "Actualizar biblioteca" } Icon { name: "arrow" } }
        }
        section { class: "settings-section",
            div { class: "settings-section__header", div { class: "settings-section__icon", Icon { name: "plus", size: 19 } } div { div { class: "settings-section__title", "Importación" } div { class: "settings-section__sub", "Cómo guardar los archivos que añades" } } }
            button { class: "settings-row", onclick: move |_| { let mut value = copy_state.settings.read().clone(); value.import_mode = "copy".into(); copy_state.save_settings(value); },
                span { class: "settings-row__label", span { class: "settings-row__title", "Copiar a la biblioteca" } span { class: "settings-row__sub", "Conserva los archivos originales" } }
                span { class: if settings.import_mode == "copy" { "settings-row__check active" } else { "settings-row__check" }, Icon { name: "check", size: 11 } }
            }
            button { class: "settings-row", onclick: move |_| { let mut value = reference_state.settings.read().clone(); value.import_mode = "reference".into(); reference_state.save_settings(value); },
                span { class: "settings-row__label", span { class: "settings-row__title", "Usar ubicación original" } span { class: "settings-row__sub", "Sin duplicar espacio; el archivo debe permanecer en su carpeta" } }
                span { class: if settings.import_mode == "reference" { "settings-row__check active" } else { "settings-row__check" }, Icon { name: "check", size: 11 } }
            }
            ToggleSetting { title: "Buscar archivos al iniciar", description: "Añade nuevos archivos de la carpeta de biblioteca", checked: settings.scan_library_on_startup, onchange: move |checked| { let mut value = scan_state.settings.read().clone(); value.scan_library_on_startup = checked; scan_state.save_settings(value); } }
        }
        section { class: "settings-section",
            div { class: "settings-section__header", div { class: "settings-section__icon", Icon { name: "settings", size: 19 } } div { div { class: "settings-section__title", "Herramientas avanzadas" } div { class: "settings-section__sub", "Procesamiento de audio" } } }
            ToggleSetting { title: "Mostrar herramientas avanzadas", description: "Muestra Stems en la barra lateral", checked: settings.show_advanced_tools, onchange: move |checked| { let mut value = advanced_state.settings.read().clone(); value.show_advanced_tools = checked; advanced_state.save_settings(value); } }
            ToggleSetting { title: "Modo desarrollador", description: "Permite administrar plugins y sus comandos", checked: settings.developer_mode, onchange: move |checked| { let mut value = developer_state.settings.read().clone(); value.developer_mode = checked; developer_state.save_settings(value); } }
        }
        if settings.developer_mode { crate::plugins::PluginPanel {} }
        crate::updates::UpdatePanel {}
        section { class: "settings-section",
            div { class: "settings-section__header", div { class: "settings-section__icon", Icon { name: "music", size: 19 } } div { div { class: "settings-section__title", "Acerca de" } div { class: "settings-section__sub", "YT Ultimate 5.0.0 · Vista previa" } } }
            p { class: "rust-settings-note", if cfg!(feature = "native-stems") { "Edición con separación de voz y herramientas de audio." } else { "Edición base para escuchar, descargar y procesar tu música." } }
        }
        div { class: "settings-footer", "YT Ultimate · Tu música, sin distracciones." }
    } }
}

#[component]
fn ToggleSetting(
    title: String,
    description: String,
    checked: bool,
    onchange: EventHandler<bool>,
) -> Element {
    rsx! { button { class: "settings-row settings-row--toggle", role: "switch", aria_checked: checked, onclick: move |_| onchange.call(!checked),
        span { class: "settings-row__label", span { class: "settings-row__title", "{title}" } span { class: "settings-row__sub", "{description}" } }
        span { class: if checked { "settings-switch active" } else { "settings-switch" }, span {} }
    } }
}

#[component]
pub fn StemsPage() -> Element {
    rsx! { crate::processing::ProcessingPage {} }
}
