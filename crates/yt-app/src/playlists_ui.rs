use crate::{
    app::{artwork_src, EmptyState, Equalizer, Thumb},
    icons::Icon,
    state::AppState,
};
use dioxus::prelude::*;
use std::collections::HashSet;
use yt_core::{AppCore, Playlist, Track};

#[component]
pub fn PlaylistsPage() -> Element {
    let state = use_context::<AppState>();
    let mut selected = state.selected_playlist;
    let mut creating = use_signal(|| false);
    let mut name = use_signal(String::new);
    let mut renaming = use_signal(|| None::<Playlist>);
    let mut deleting = use_signal(|| None::<Playlist>);
    let mut dragging = use_signal(|| None::<String>);
    let playlists = state.playlists.read().clone();
    let _library = state.library.read();
    let selection = selected
        .read()
        .clone()
        .and_then(|id| playlists.iter().find(|playlist| playlist.id == id).cloned());
    let tracks_result = selection
        .as_ref()
        .map(|playlist| state.services.core.playlist_tracks(&playlist.id));
    let load_error = tracks_result
        .as_ref()
        .and_then(|value| value.as_ref().err())
        .map(ToString::to_string);
    let tracks = tracks_result.and_then(Result::ok).unwrap_or_default();
    let create_state = state.clone();
    rsx! {
        style {{include_str!("playlists_ui.css")}}
        div {class:"split-page",
            div {class:"split-page__top",
                div {class:"toolbar playlist-toolbar",div {class:"toolbar__left",h1 {class:"page-title","Playlists"}span {class:"page-count","{playlists.len()}"}}
                    div {class:"toolbar__right",button {class:"btn-secondary",aria_label:"Crear nueva playlist".to_owned(),onclick:move |_|{let next=!*creating.peek();creating.set(next);if !next{name.set(String::new());}},Icon{name:"plus",size:13}"Nueva"}}
                }
                if *creating.read(){
                    form {class:"anim-fade playlist-create",onsubmit:move |event|{
                        event.prevent_default();let title=name.peek().trim().to_owned();
                        match create_state.services.core.create_playlist(&title){Ok(playlist)=>{selected.set(Some(playlist.id));name.set(String::new());creating.set(false);create_state.refresh();create_state.notify_success("Playlist creada");},Err(error)=>create_state.notify_error(error.to_string())}
                    },onkeydown:move |event|{if event.key()==Key::Escape{event.stop_propagation();creating.set(false);name.set(String::new());}},
                        div {class:"input-search-wrap",input {class:"input-search playlist-name-input",placeholder:"Nombre de la playlist…",aria_label:"Nombre de la nueva playlist",value:"{name}",onmounted:|event|async move{let _=event.set_focus(true).await;},oninput:move |event|name.set(event.value())}}
                        button {class:"btn-accent",r#type:"submit",disabled:name.read().trim().is_empty(),"Crear"}
                        button {class:"btn-icon",r#type:"button",title:"Cancelar",onclick:move |_|{creating.set(false);name.set(String::new());},Icon{name:"close",size:14}}
                    }
                }
            }
            div {class:"split-page__body",
                aside {class:"playlist-rail",
                    if playlists.is_empty(){EmptyState {title:"Sin playlists",hint:"Crea la primera con «Nueva».",icon:"playlist",compact:true}}
                    for playlist in playlists {
                        PlaylistRailRow {key:"{playlist.id}",playlist:playlist.clone(),active:selection.as_ref().is_some_and(|value|value.id==playlist.id),onselect:move |id|{selected.set(Some(id));dragging.set(None);},ondelete:move |playlist|deleting.set(Some(playlist))}
                    }
                }
                div {class:"split-page__detail",
                    if let Some(playlist)=selection {
                        div {class:"anim-fade",key:"{playlist.id}",
                            div {class:"collection-hero",
                                div {class:"playlist-hero-cover",PlaylistCover {tracks:tracks.clone(),size:"xl"}}
                                div {class:"collection-hero__meta",span {class:"collection-hero__eyebrow","Playlist"}span {class:"collection-hero__title","{playlist.name}"}span {class:"collection-hero__sub","{track_count_label(tracks.len())}"}
                                    div {class:"collection-hero__actions",
                                        button {class:"btn-accent",disabled:tracks.is_empty(),onclick:{let state=state.clone();let tracks=tracks.clone();move |_|state.play_all(tracks.clone(),false)},Icon{name:"play",size:13,fill:"currentColor"}"Reproducir"}
                                        button {class:"btn-secondary",title:"Reproducir en orden aleatorio",disabled:tracks.is_empty(),onclick:{let state=state.clone();let tracks=tracks.clone();move |_|state.play_all(tracks.clone(),true)},Icon{name:"shuffle",size:13}}
                                        button {class:"btn-secondary",disabled:tracks.is_empty(),onclick:{let state=state.clone();let playlist=playlist.clone();move |_|state.export_playlist(playlist.clone())},Icon{name:"download",size:13}"Exportar"}
                                        details {class:"playlist-options",
                                            summary {class:"btn-icon",title:"Opciones de playlist",aria_label:"Opciones de playlist",Icon{name:"more",size:16}}
                                            div {class:"playlist-options-menu",
                                                button {class:"settings-row",onclick:{let playlist=playlist.clone();move |_|renaming.set(Some(playlist.clone()))},"Renombrar…"}
                                                button {class:"settings-row playlist-delete",onclick:{let playlist=playlist.clone();move |_|deleting.set(Some(playlist.clone()))},Icon{name:"trash",size:13}"Eliminar playlist…"}
                                            }
                                        }
                                    }
                                }
                            }
                            if let Some(error)=load_error {p {class:"task-row__error",role:"alert","No se pudieron leer las canciones: {error}"}}
                            else if tracks.is_empty(){EmptyState {title:"Playlist vacía",hint:"Añade canciones desde Canciones con clic derecho.",icon:"music",compact:true}}
                            else {for (index,track) in tracks.iter().cloned().enumerate(){PlaylistTrack {key:"{track.id}",playlist_id:playlist.id.clone(),track,index,total:tracks.len(),dragging}}}
                        }
                    }else{EmptyState {title:"Selecciona una playlist",hint:"Añade canciones desde Canciones con clic derecho → «Añadir a playlist».",icon:"playlist"}}
                }
            }
        }
        if let Some(playlist)=renaming.read().clone(){
            PlaylistNameDialog {key:"{playlist.id}",playlist:playlist.clone(),onclose:move |_|renaming.set(None),onsave:{let state=state.clone();move |name:String|match state.services.core.rename_playlist(&playlist.id,&name){Ok(())=>{renaming.set(None);state.refresh();state.notify_success("Playlist renombrada");},Err(error)=>state.notify_error(error.to_string())}}}
        }
        if let Some(playlist)=deleting.read().clone(){
            DeletePlaylistDialog {playlist:playlist.clone(),onclose:move |_|deleting.set(None),onconfirm:{let state=state.clone();move |_|match state.services.core.delete_playlist(&playlist.id){Ok(())=>{let clear=selected.peek().as_deref()==Some(&playlist.id);if clear{selected.set(None);}deleting.set(None);state.refresh();state.notify_success("Playlist eliminada");},Err(error)=>state.notify_error(error.to_string())}}}
        }
    }
}

#[component]
fn PlaylistRailRow(
    playlist: Playlist,
    active: bool,
    onselect: EventHandler<String>,
    ondelete: EventHandler<Playlist>,
) -> Element {
    let state = use_context::<AppState>();
    let _library = state.library.read();
    let tracks = state
        .services
        .core
        .playlist_tracks(&playlist.id)
        .unwrap_or_default();
    let id = playlist.id.clone();
    let remove = playlist.clone();
    rsx! {div {class:if active{"row row--active playlist-rail-row"}else{"row playlist-rail-row"},
        button {class:"playlist-rail-select",onclick:move |_|onselect.call(id.clone()),aria_label:format!("Abrir playlist {}",playlist.name),aria_pressed:format!("{active}"),
            PlaylistCover {tracks}
            div {class:"row__meta",div {class:if active{"row__title row__title--active"}else{"row__title"},"{playlist.name}"}div {class:"row__sub","{track_count_label(playlist.track_count as usize)}"}}
        }
        div {class:"row__actions",button {class:"btn-icon-sm btn-icon--danger",title:"Eliminar playlist",aria_label:format!("Eliminar playlist {}",playlist.name),onclick:move |_|ondelete.call(remove.clone()),Icon{name:"trash",size:12}}}
    }}
}

#[component]
fn PlaylistCover(tracks: Vec<Track>, #[props(default = "xs")] size: &'static str) -> Element {
    let state = use_context::<AppState>();
    let core = state.services.core.clone();
    let covers = use_resource(use_reactive((&tracks,), move |(tracks,)| {
        let core = core.clone();
        async move {
            tokio::task::spawn_blocking(move || cover_sources(&core, &tracks))
                .await
                .unwrap_or_default()
        }
    }));
    let sources = covers.read().clone().unwrap_or_default();
    rsx! {PlaylistCoverView {sources,size}}
}
#[component]
fn PlaylistCoverView(sources: Vec<String>, size: &'static str) -> Element {
    let mut failed = use_signal(HashSet::<String>::new);
    let sources = sources
        .into_iter()
        .filter(|source| !failed.read().contains(source))
        .collect::<Vec<_>>();
    if sources.len() >= 4 {
        rsx! {div {class:"thumb thumb--{size}",div {class:"mosaic",for source in sources.into_iter().take(4){img {key:"{source}",src:source.clone(),alt:"",loading:"lazy",onerror:move |_|{failed.write().insert(source.clone());}}}}}}
    } else {
        rsx! {Thumb {src:sources.first().cloned().unwrap_or_default(),size,icon_size:Some(if size=="xl"{30}else{12})}}
    }
}
fn cover_sources(core: &AppCore, tracks: &[Track]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut covers = Vec::new();
    for track in tracks {
        let candidate = if track.artwork_url.starts_with("https://") {
            Some((track.artwork_url.clone(), track.artwork_url.clone()))
        } else {
            core.artwork_path(&track.id).ok().flatten().map(|path| {
                (
                    path.to_string_lossy().into_owned(),
                    format!("/artwork/{}", track.id),
                )
            })
        };
        if let Some((identity, url)) = candidate {
            if seen.insert(identity) {
                covers.push(url);
                if covers.len() == 4 {
                    break;
                }
            }
        }
    }
    covers
}

#[component]
fn PlaylistTrack(
    playlist_id: String,
    track: Track,
    index: usize,
    total: usize,
    mut dragging: Signal<Option<String>>,
) -> Element {
    let state = use_context::<AppState>();
    let drag_id = track.id.clone();
    let drop_id = track.id.clone();
    let remove_id = track.id.clone();
    let up_id = track.id.clone();
    let down_id = track.id.clone();
    let move_label = format!("Mover {}", track.title);
    let active = state
        .current
        .read()
        .as_ref()
        .is_some_and(|value| value.id == track.id);
    let play_state = state.clone();
    let play_track = track.clone();
    let menu_state = state.clone();
    let menu_track = track.clone();
    let menu_playlist = playlist_id.clone();
    let native_state = state.clone();
    let native_track = track.clone();
    rsx! {div {class:if active{"row row--active playlist-track-wrap"}else{"row playlist-track-wrap"},
        oncontextmenu:move|event|crate::context_menu::show(event,playlist_menu(menu_state.clone(),menu_track.clone(),menu_playlist.clone())),
        draggable:!track.missing&&!track.path.as_os_str().is_empty(),
        ondragstart:move|event|{event.prevent_default();event.stop_propagation();crate::native::begin_media_drag(&native_state,&native_track);},
        ondragover:move |event|{if dragging.peek().is_some(){event.prevent_default();event.stop_propagation();}},ondrop:{let state=state.clone();let playlist_id=playlist_id.clone();move |event|{
            let source=dragging.peek().clone();if let Some(source)=source{event.prevent_default();event.stop_propagation();dragging.set(None);let result=state.services.core.playlist_tracks(&playlist_id).and_then(|tracks|{let mut ids=tracks.into_iter().map(|track|track.id).collect::<Vec<_>>();move_before(&mut ids,&source,&drop_id);state.services.core.reorder_playlist(&playlist_id,&ids)});match result{Ok(())=>state.refresh(),Err(error)=>state.notify_error(error.to_string())}}
        }},
        button {class:"btn-icon-sm playlist-drag-handle",title:"Arrastrar para reordenar",aria_label:move_label,draggable:true,ondragstart:move |event|{event.stop_propagation();dragging.set(Some(drag_id.clone()));},ondragend:move |_|dragging.set(None),Icon{name:"list",size:12}}
        button {class:"playlist-track-play",onclick:move |_|{if active{play_state.toggle_play();}else{play_state.play(play_track.clone());}},
            span {class:"row__index",if active&&*state.playing.read(){Equalizer{}}else{"{index+1}"}}
            Thumb {src:artwork_src(&track),size:"xs",icon_size:Some(12)}
            div {class:"row__meta",div {class:if active{"row__title row__title--active"}else{"row__title"},"{track.title}"}div {class:"row__sub",if track.artist.is_empty(){"{crate::state::size_label(track.size_bytes)}"}else{"{track.artist}"}}}
        }
        div {class:"playlist-order-actions",
            button {class:"btn-icon-sm",title:"Mover arriba",disabled:index==0,onclick:{let state=state.clone();let playlist_id=playlist_id.clone();move |_|move_step(&state,&playlist_id,&up_id,-1)},"↑"}
            button {class:"btn-icon-sm",title:"Mover abajo",disabled:index+1>=total,onclick:{let state=state.clone();let playlist_id=playlist_id.clone();move |_|move_step(&state,&playlist_id,&down_id,1)},"↓"}
        }
        div {class:"row__actions",
            button {class:"btn-icon-sm",title:"Quitar de la playlist",onclick:move |_|match state.services.core.remove_from_playlist(&playlist_id,&remove_id){Ok(())=>state.refresh(),Err(error)=>state.notify_error(error.to_string())},Icon{name:"close",size:13}}
        }
    }}
}
fn playlist_menu(
    state: AppState,
    track: Track,
    playlist_id: String,
) -> Vec<crate::context_menu::Entry> {
    use crate::context_menu::Entry;
    let shared = crate::context_menu::track_items(state.clone(), track.clone());
    let mut items = shared
        .iter()
        .filter(|item| {
            matches!(
                item.label.as_str(),
                "Reproducir" | "Reproducir a continuación" | "Agregar a la cola"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    items.push(Entry::separator());
    items.push(Entry::action("Quitar de la playlist", move || match state
        .services
        .core
        .remove_from_playlist(&playlist_id, &track.id)
    {
        Ok(()) => state.refresh(),
        Err(error) => state.notify_error(error.to_string()),
    }));
    items.push(Entry::separator());
    items.extend(
        shared
            .iter()
            .filter(|item| item.label == "Versión Alvin y las ardillas…")
            .cloned(),
    );
    items.push(Entry::separator());
    items.extend(shared.into_iter().filter(|item| {
        item.label.starts_with("Mostrar en ")
            || matches!(
                item.label.as_str(),
                "Abrir con la app predeterminada" | "Copiar ruta"
            )
    }));
    items
}

fn move_step(state: &AppState, playlist: &str, track: &str, offset: isize) {
    let result = state
        .services
        .core
        .playlist_tracks(playlist)
        .and_then(|tracks| {
            let mut ids = tracks.into_iter().map(|track| track.id).collect::<Vec<_>>();
            if let Some(index) = ids.iter().position(|id| id == track) {
                let next = index as isize + offset;
                if next >= 0 && (next as usize) < ids.len() {
                    ids.swap(index, next as usize);
                }
            }
            state.services.core.reorder_playlist(playlist, &ids)
        });
    match result {
        Ok(()) => state.refresh(),
        Err(error) => state.notify_error(error.to_string()),
    }
}
fn move_before(ids: &mut Vec<String>, source: &str, destination: &str) {
    if source == destination {
        return;
    }
    let Some(from) = ids.iter().position(|id| id == source) else {
        return;
    };
    if !ids.iter().any(|id| id == destination) {
        return;
    }
    let item = ids.remove(from);
    if let Some(to) = ids.iter().position(|id| id == destination) {
        ids.insert(to, item);
    }
}
fn track_count_label(count: usize) -> String {
    format!(
        "{count} {}",
        if count == 1 { "canción" } else { "canciones" }
    )
}

#[component]
fn PlaylistNameDialog(
    playlist: Playlist,
    onclose: EventHandler<()>,
    onsave: EventHandler<String>,
) -> Element {
    let mut name = use_signal(|| playlist.name.clone());
    rsx! {div {class:"modal-backdrop",onclick:move |_|onclose.call(()),
        form {class:"modal-panel playlist-name-dialog",role:"dialog",aria_modal:"true",aria_label:"Renombrar playlist",onclick:|event|event.stop_propagation(),onkeydown:move |event|{if event.key()==Key::Escape{event.stop_propagation();onclose.call(());}},onsubmit:move |event|{event.prevent_default();let title=name.peek().trim().to_owned();if !title.is_empty(){onsave.call(title);}},
            div {class:"modal-header",div {class:"modal-title","Renombrar playlist"}button {r#type:"button",class:"btn-icon-sm",title:"Cerrar",onclick:move |_|onclose.call(()),Icon{name:"close",size:14}}}
            div {class:"input-search-wrap",input {class:"input-search playlist-name-input",aria_label:"Nuevo nombre de playlist",value:"{name}",onmounted:|event|async move{let _=event.set_focus(true).await;},oninput:move |event|name.set(event.value())}}
            div {class:"modal-actions",button {r#type:"button",class:"btn-secondary",onclick:move |_|onclose.call(()),"Cancelar"}button {r#type:"submit",class:"btn-accent",disabled:name.read().trim().is_empty(),"Guardar"}}
        }
    }}
}
#[component]
fn DeletePlaylistDialog(
    playlist: Playlist,
    onclose: EventHandler<()>,
    onconfirm: EventHandler<()>,
) -> Element {
    rsx! {div {class:"modal-backdrop",onclick:move |_|onclose.call(()),
        div {class:"modal-panel playlist-name-dialog",role:"alertdialog",aria_modal:"true",aria_label:"Eliminar playlist",onclick:|event|event.stop_propagation(),onkeydown:move |event|{if event.key()==Key::Escape{event.stop_propagation();onclose.call(());}},
            div {class:"modal-header",div {class:"modal-title","¿Eliminar la playlist «{playlist.name}»?"}}
            p {class:"playlist-delete-hint","Se quitará la playlist. Las canciones de tu biblioteca se conservan."}
            div {class:"modal-actions",button {class:"btn-secondary",onmounted:|event|async move{let _=event.set_focus(true).await;},onclick:move |_|onclose.call(()),"Cancelar"}button {class:"btn-accent playlist-delete-button",onclick:move |_|onconfirm.call(()),"Eliminar playlist"}}
        }
    }}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{use_app_state, Services};
    use dioxus::dioxus_core::{AttributeValue, ElementId, Mutation, Mutations};
    use std::rc::Rc;

    fn harness() -> Element {
        let services = use_context::<Rc<Services>>();
        let state = use_app_state(services);
        use_context_provider(|| state);
        rsx! { PlaylistsPage {} }
    }
    fn click(dom: &VirtualDom, id: ElementId) {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let data = dioxus::html::PlatformEventData::new(Box::new(
            dioxus::html::SerializedMouseData::default(),
        ));
        dom.runtime()
            .handle_event("click", Event::new(Rc::new(data), true), id);
    }
    fn labeled(mutations: &Mutations, label: &str) -> ElementId {
        mutations
            .edits
            .iter()
            .find_map(|mutation| match mutation {
                Mutation::SetAttribute {
                    name: "aria-label",
                    value: AttributeValue::Text(text),
                    id,
                    ..
                } if text == label => Some(*id),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing labeled control {label}"))
    }

    #[tokio::test]
    async fn playlist_selection_creation_and_delete_confirmation_keep_library_data() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let track = services
            .core
            .import_bytes(
                "Canción.wav",
                include_bytes!("../../yt-quality/tests/fixtures/input.wav"),
            )
            .unwrap();
        let playlist = services.core.create_playlist("Favoritas").unwrap();
        services
            .core
            .add_to_playlist(&playlist.id, &track.id)
            .unwrap();
        let mut dom = VirtualDom::new(harness);
        dom.insert_any_root_context(Box::new(services.clone()));
        let mutations = dom.rebuild_to_vec();
        let new_id = labeled(&mutations, "Crear nueva playlist");
        let select_id = labeled(&mutations, "Abrir playlist Favoritas");
        let delete_id = labeled(&mutations, "Eliminar playlist Favoritas");
        click(&dom, select_id);
        dom.render_immediate_to_vec();
        let html = dioxus_ssr::render(&dom);
        for text in [
            "collection-hero",
            "1 canción",
            "Reproducir en orden aleatorio",
            "Exportar",
            "Renombrar…",
            "Arrastrar para reordenar",
            "Mover arriba",
            "Quitar de la playlist",
        ] {
            assert!(html.contains(text), "missing playlist control {text}");
        }
        click(&dom, new_id);
        dom.render_immediate_to_vec();
        let html = dioxus_ssr::render(&dom);
        assert!(html.contains("Nombre de la nueva playlist"));
        assert!(html.contains("Cancelar"));
        click(&dom, new_id);
        dom.render_immediate_to_vec();
        assert!(!dioxus_ssr::render(&dom).contains("Nombre de la nueva playlist"));
        click(&dom, delete_id);
        dom.render_immediate_to_vec();
        let html = dioxus_ssr::render(&dom);
        assert!(html.contains("role=\"alertdialog\""));
        assert!(html.contains("Las canciones de tu biblioteca se conservan."));
        assert_eq!(services.core.playlists().unwrap().len(), 1);
        assert_eq!(services.core.library("").unwrap().len(), 1);
    }

    #[test]
    fn moving_playlist_entries_preserves_all_ids_and_handles_stale_drag_sources() {
        let mut ids = ["a", "b", "c", "d"].map(str::to_owned).to_vec();
        move_before(&mut ids, "d", "b");
        assert_eq!(ids, ["a", "d", "b", "c"]);
        move_before(&mut ids, "a", "c");
        assert_eq!(ids, ["d", "b", "a", "c"]);
        let current = ids.clone();
        for (source, destination) in [("a", "a"), ("missing", "c"), ("b", "missing")] {
            move_before(&mut ids, source, destination);
            assert_eq!(ids, current);
        }
    }

    #[test]
    fn cover_collection_deduplicates_real_artwork_and_limits_mosaic_to_four() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let source = profile.path().join("source.wav");
        std::fs::write(
            &source,
            include_bytes!("../../yt-quality/tests/fixtures/input.wav"),
        )
        .unwrap();
        let cover = core
            .cache_artwork(include_bytes!("../../yt-core/tests/fixtures/cover.png"))
            .unwrap();
        let mut tracks = Vec::new();
        for index in 0..2 {
            let output = profile.path().join(format!("tagged-{index}.wav"));
            yt_audio::convert_with_metadata(
                &source,
                &output,
                yt_audio::AudioFormat::Wav,
                &yt_core::MediaMetadata {
                    artwork_path: Some(cover.clone()),
                    ..Default::default()
                },
                &yt_media::CancellationToken::new(),
                |_| {},
            )
            .unwrap();
            tracks.push(
                core.import_file(&output, yt_core::ImportMode::Reference)
                    .unwrap(),
            );
        }
        for index in 0..5 {
            let mut remote = tracks[0].clone();
            remote.artwork_url = format!("https://i.ytimg.com/{index}.jpg");
            tracks.push(remote.clone());
            tracks.push(remote);
        }
        let covers = cover_sources(&core, &tracks);
        assert_eq!(covers.len(), 4);
        assert_eq!(covers[0], format!("/artwork/{}", tracks[0].id));
        assert_eq!(
            covers[1..],
            [
                "https://i.ytimg.com/0.jpg",
                "https://i.ytimg.com/1.jpg",
                "https://i.ytimg.com/2.jpg"
            ]
        );
        fn mosaic() -> Element {
            rsx! {PlaylistCoverView {sources:(0..4).map(|n|format!("https://i.ytimg.com/{n}.jpg")).collect::<Vec<_>>(),size:"xl"}}
        }
        let mut dom = VirtualDom::new(mosaic);
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert!(html.contains("class=\"mosaic\""));
        assert_eq!(html.matches("<img ").count(), 4);
    }
}
