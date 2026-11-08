use crate::{
    app::{artwork_src, EmptyState, Equalizer, Thumb},
    context_menu::{self, Entry},
    icons::Icon,
    state::{size_label, AppState},
};
use dioxus::prelude::*;
use yt_core::Track;

fn move_item(state: &AppState, from: usize, to: usize) {
    let mut signal = state.queue;
    {
        let mut items = signal.write();
        if from >= items.len() || to >= items.len() || from == to {
            return;
        }
        let item = items.remove(from);
        items.insert(to, item);
    }
    state.persist_playback();
}
fn play_item(state: &AppState, index: usize) {
    let mut signal = state.queue;
    let item = {
        let mut items = signal.write();
        if index >= items.len() {
            return;
        }
        items.remove(index)
    };
    state.play(item);
    state.persist_playback();
}
fn remove_item(state: &AppState, index: usize) {
    let mut signal = state.queue;
    {
        let mut items = signal.write();
        if index >= items.len() {
            return;
        }
        items.remove(index);
    }
    state.persist_playback();
}
#[component]
pub fn QueuePanel() -> Element {
    let state = use_context::<AppState>();
    let current = state.current.read().clone();
    let queue = state.queue.read().clone();
    let history = state
        .history
        .read()
        .iter()
        .filter(|item| current.as_ref().is_none_or(|current| current.id != item.id))
        .take(12)
        .cloned()
        .collect::<Vec<_>>();
    let mut close = state.clone();
    let mut dismiss = state.clone();
    let mut clear = state.clone();
    let dragging = use_signal(|| None::<usize>);
    rsx! {div{class:"queue-backdrop",onclick:move |_|close.queue_open.set(false)}
        aside{class:"queue-panel",role:"dialog",aria_label:"Cola de reproducción",
            div{class:"queue-panel__header",span{class:"queue-panel__title","Cola"}div{style:"display:flex;gap:4px;align-items:center",
                if !queue.is_empty(){button{class:"btn-ghost",style:"font-size:12px;padding:4px 8px",onclick:move |_|{clear.queue.write().clear();clear.persist_playback();},"Vaciar"}}
                button{class:"btn-icon-sm",title:"Cerrar (Esc)",onclick:move |_|dismiss.queue_open.set(false),Icon{name:"close",size:15}}
            }}
            div{class:"queue-panel__scroll",
                if let Some(track)=current.clone(){div{class:"queue-label","Reproduciendo"}div{class:"row row--active",style:"cursor:default",
                    Thumb{src:artwork_src(&track),size:"sm",icon_size:Some(14),video:track.media_type=="video",if *state.playing.read(){div{class:"thumb-playing",Equalizer{}}}}
                    div{class:"row__meta",div{class:"row__title row__title--active",style:"display:flex;align-items:center;gap:5px",if crate::remote::is_remote(&track){Icon{name:"radio",size:11}}span{style:"overflow:hidden;text-overflow:ellipsis","{track.title}"}}div{class:"row__sub",if track.artist.is_empty(){"{size_label(track.size_bytes)}"}else{"{track.artist}"}}}
                }}
                if !queue.is_empty(){div{class:"queue-label","A continuación"}for(index,track)in queue.iter().cloned().enumerate(){QueueRow{key:"{index}-{track.id}",track,index,dragging}}}
                if !history.is_empty(){div{class:"queue-label","Historial"}for(index,track)in history.iter().cloned().enumerate(){div{key:"history-{index}-{track.id}",class:"row queue-row-history",title:"Volver a reproducir",onclick:{let state=state.clone();let track=track.clone();move |_|state.play(track.clone())},Thumb{src:artwork_src(&track),size:"xs",icon_size:Some(12)}div{class:"row__meta",div{class:"row__title","{track.title}"}div{class:"row__sub","{track.artist}"}}}}}
                if current.is_none()&&queue.is_empty()&&history.is_empty(){EmptyState{title:"Cola vacía",hint:"Reproduce algo o añade canciones con clic derecho → «Reproducir a continuación».",icon:"playlist",compact:true}}
            }
        }
    }
}
#[component]
fn QueueRow(track: Track, index: usize, mut dragging: Signal<Option<usize>>) -> Element {
    let state = use_context::<AppState>();
    let play = state.clone();
    let remove = state.clone();
    let drop_state = state.clone();
    let menu = state.clone();
    rsx! {div{class:if *dragging.read()==Some(index){"row row--dragging"}else{"row"},title:"Reproducir ahora",draggable:true,
        onclick:move |_|play_item(&play,index),
        oncontextmenu:move|event|{let play=menu.clone();let first=menu.clone();let remove=menu.clone();context_menu::show(event,vec![Entry::action("Reproducir ahora",move||play_item(&play,index)),Entry::action("Mover al inicio",move||move_item(&first,index,0)).enabled(index>0),Entry::separator(),Entry::action("Quitar de la cola",move||remove_item(&remove,index))]);},
        ondragstart:move|event|{event.stop_propagation();dragging.set(Some(index));},ondragover:|event|{event.prevent_default();event.stop_propagation();},ondragend:move |_|dragging.set(None),
        ondrop:move|event|{event.prevent_default();event.stop_propagation();let source=*dragging.peek();dragging.set(None);if let Some(from)=source{move_item(&drop_state,from,index);}},
        Thumb{src:artwork_src(&track),size:"xs",icon_size:Some(12)}
        div{class:"row__meta",div{class:"row__title",style:"display:flex;align-items:center;gap:5px",if crate::remote::is_remote(&track){Icon{name:"radio",size:10}}span{style:"overflow:hidden;text-overflow:ellipsis","{track.title}"}}div{class:"row__sub","{track.artist}"}}
        div{class:"row__actions",button{class:"btn-icon-sm",title:"Quitar de la cola",onclick:move|event|{event.stop_propagation();remove_item(&remove,index);},Icon{name:"close",size:13}}}
    }}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{use_app_state, Services};
    use std::{cell::RefCell, rc::Rc};
    type Capture = Rc<RefCell<Option<AppState>>>;
    fn harness() -> Element {
        let services = use_context::<Rc<Services>>();
        let capture = use_context::<Capture>();
        let state = use_app_state(services);
        *capture.borrow_mut() = Some(state.clone());
        use_context_provider(|| state);
        rsx! {QueuePanel{}}
    }
    #[tokio::test]
    async fn reorder_duplicate_entries_play_and_remove_persist_the_exact_queue() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let first = services.core.import_bytes("first.mp4", &[1, 2, 3]).unwrap();
        let second = services
            .core
            .import_bytes("second.mp4", &[4, 5, 6])
            .unwrap();
        let capture: Capture = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(harness);
        dom.insert_any_root_context(Box::new(services.clone()));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let mut state = capture.borrow().as_ref().unwrap().clone();
        dom.in_scope(ScopeId::ROOT, || {
            state
                .queue
                .set(vec![first.clone(), second.clone(), first.clone()]);
            move_item(&state, 2, 0);
            assert_eq!(
                state.queue.peek().iter().map(|t| &t.id).collect::<Vec<_>>(),
                vec![&first.id, &first.id, &second.id]
            );
            move_item(&state, 7, 0);
            remove_item(&state, 1);
            assert_eq!(
                services.core.load_playback().unwrap().queue,
                vec![first.id.clone(), second.id.clone()]
            );
            play_item(&state, 1);
            assert_eq!(state.current.peek().as_ref().unwrap().id, second.id);
            assert_eq!(state.queue.peek()[0].id, first.id);
            let saved = services.core.load_playback().unwrap();
            assert_eq!(saved.queue, vec![second.id.clone(), first.id.clone()]);
            assert_eq!(saved.current_index, Some(0));
            remove_item(&state, 99);
            play_item(&state, 99);
            assert_eq!(state.queue.peek().len(), 1);
        });
    }
}
