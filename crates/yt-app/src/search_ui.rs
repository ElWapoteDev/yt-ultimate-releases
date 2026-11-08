use crate::{
    app::{EmptyState, Equalizer, Thumb},
    context_menu::Entry,
    icons::Icon,
    state::{time_label, AppState, Page},
};
use chrono::Datelike;
use dioxus::prelude::*;
use yt_media::SearchResult;

#[component]
pub fn SearchPage() -> Element {
    let s = use_context::<AppState>();
    let mut query = use_signal(String::new);
    let mut chip = use_signal(|| None::<&'static str>);
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
    let mut results = use_signal(Vec::<SearchResult>::new);
    let mut playlist = use_signal(|| None::<(String, String, String, bool)>);
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
            let youtube = match state.services.youtube_client() {
                Ok(client) => client,
                Err(error) => {
                    loading.set(false);
                    state.notify(error.to_string());
                    return;
                }
            };
            if url::Url::parse(&term)
                .is_ok_and(|url| url.query_pairs().any(|(key, _)| key == "list"))
            {
                let response = youtube.playlist(&term).await;
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
                youtube.metadata(&term).await.map(|item| vec![item])
            } else {
                youtube.search(&term, 25).await
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
        ("Reggaeton", format!("reggaeton {year} mix")),
        ("Rock", "classic rock greatest hits".into()),
        ("Lo-Fi", "lofi hip hop beats".into()),
        ("Electrónica", "electronic dance music".into()),
        ("Jazz", "jazz classics playlist".into()),
        ("K-Pop", format!("kpop hits {year}")),
    ];
    let rows = results.read().clone();
    let collection = playlist.read().clone();
    rsx! { div { class: "page-shell",
        div { class:"page-header", h1 { class:"page-title","Buscar" } }
        div { style:"display:flex;flex-direction:column;gap:14px;margin-bottom:28px",
            form { onsubmit:move|event|{event.prevent_default();chip.set(None);submit_search(query.peek().clone());},
                div {class:"input-search-wrap",span{class:"input-search-icon",Icon{name:"search",size:15}}
                    input {id:"ytu-search",class:"input-search input-search--hero",placeholder:"Canciones, artistas o enlaces de YouTube",aria_label:"Buscar música",value:"{query}",autofocus:true,onmounted:move|event|input.set(Some(event.data())),oninput:move|event|query.set(event.value())}
                    if !query.read().is_empty(){button{r#type:"button",class:"input-search-clear",aria_label:"Limpiar",onclick:move |_|{query.set(String::new());results.set(Vec::new());searched.set(false);chip.set(None);playlist.set(None);generation+=1;loading.set(false);let element=input.peek().clone();spawn(async move{if let Some(element)=element{let _=element.set_focus(true).await;}});},"×"}}
                }
            }
            div{class:"chip-row",for(label,term)in categories{button{class:if *chip.read()==Some(label){"chip active"}else{"chip"},onclick:{let mut search=run_search.clone();move |_|{query.set(String::new());chip.set(Some(label));search(term.clone());}},"{label}"}}}
        }
        if *loading.read(){ EmptyState{title:if query.read().contains("list="){"Cargando playlist…"}else{"Buscando…"},hint:"",icon:"loading"} }
        else if let Some((title,thumbnail,channel,truncated))=collection{
            div{class:"anim-fade",div{class:"collection-hero",Thumb{src:thumbnail,size:"xl",icon_size:Some(30)}
                div{class:"collection-hero__meta",span{class:"collection-hero__eyebrow","Playlist de YouTube"}span{class:"collection-hero__title","{title}"}span{class:"collection-hero__sub",if !channel.is_empty(){"{channel} · "}"{rows.len()} vídeos"}
                    div{class:"collection-hero__actions",for(label,video)in[("Descargar todo en MP3",false),("MP4",true)]{button{class:if video{"btn-secondary"}else{"btn-accent"},onclick:{let state=s.clone();let items=rows.clone();let name=title.clone();move |_|state.download_playlist(&name,items.clone(),video)},if !video{Icon{name:"folder-down",size:14}}"{label}"}}}
                }
            }
            if truncated{p{class:"rust-settings-note","Se alcanzó el límite de resultados. Se descargarán únicamente los vídeos mostrados."}}
            for(index,item)in rows.into_iter().enumerate(){PlaylistSearchRow{key:"{index}-{item.id}",item,index}}
            }
        }
        else if !rows.is_empty(){div{class:"anim-fade",div{class:"section-header",h2{class:"section-title",{chip.read().unwrap_or("Resultados")}}span{class:"section-count","{rows.len()}"}}
            for item in rows{SearchRow{key:"{item.id}",item}}
        }}
        else if *searched.read(){EmptyState{title:"Sin resultados",hint:"Intenta con otros términos o pega un enlace de YouTube.",icon:"search"}}
        else{EmptyState{title:"¿Qué quieres escuchar?",hint:"Escribe y pulsa Enter, pega un enlace de YouTube, o explora un género. Toca un resultado para escucharlo al instante, sin descargar.",icon:"search"}}
    } }
}

#[component]
fn SearchRow(item: SearchResult) -> Element {
    let state = use_context::<AppState>();
    let active = state
        .current
        .read()
        .as_ref()
        .is_some_and(|track| track.id == format!("youtube:{}", item.id));
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
    rsx! {div{class:if active{"row row--active"}else{"row"},title:"Reproducir sin descargar",onclick:move |_|row_state.play_remote(row_item.clone(),false),oncontextmenu:move|event|crate::context_menu::show(event,search_items(menu_state.clone(),menu_item.clone())),
        Thumb{src:item.thumbnail.clone(),size:"lg",
            if item.duration_seconds>0{span{class:"thumb-duration","{time_label(item.duration_seconds as f64)}"}}
            if playing{div{class:"thumb-playing",Equalizer{}}}
        }
        div{class:"row__meta",div{class:if active{"row__title row__title--active"}else{"row__title"},"{item.title}"}div{class:"row__sub","{item.channel}" if item.view_count>0{" · {item.view_count} visualizaciones"}}}
        div{class:"row__actions",
            button{class:"btn-icon",title:if playing{"Pausar"}else{"Reproducir (stream)"},disabled:loading,onclick:move|event|{event.stop_propagation();play_state.play_remote(play_item.clone(),false);},Icon{name:if loading{"loading"}else if playing{"pause"}else{"play"},size:14}}
            button{class:"btn-icon",title:"Reproducir a continuación",onclick:move|event|{event.stop_propagation();next_state.queue_remote(next_item.clone());},Icon{name:"list-plus",size:14}}
            DownloadActions{item}
        }
    }}
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
        Entry::separator(),
    ];
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
