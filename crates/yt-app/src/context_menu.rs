//! One Rust menu model drives native AppKit menus and the original Windows menu UI.
use crate::{icons::Icon, state::AppState};
use dioxus::prelude::*;
use std::{cell::Cell, rc::Rc};
#[cfg(target_os = "macos")]
use std::{cell::RefCell, collections::HashMap};
use yt_core::Track;

#[derive(Clone)]
pub struct Entry {
    pub label: String,
    pub enabled: bool,
    pub danger: bool,
    pub checked: Option<bool>,
    pub children: Vec<Entry>,
    action: Option<Rc<dyn Fn()>>,
    separator: bool,
}
impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.label == other.label
            && self.enabled == other.enabled
            && self.danger == other.danger
            && self.checked == other.checked
            && self.separator == other.separator
            && self.children == other.children
            && match (&self.action, &other.action) {
                (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}
impl Entry {
    pub fn action(label: impl Into<String>, action: impl Fn() + 'static) -> Self {
        Self {
            label: label.into(),
            enabled: true,
            danger: false,
            checked: None,
            children: vec![],
            action: Some(Rc::new(action)),
            separator: false,
        }
    }
    pub fn submenu(label: impl Into<String>, children: Vec<Entry>) -> Self {
        Self {
            children: normalize(children),
            ..Self::action(label, || {})
        }
    }
    pub fn separator() -> Self {
        Self {
            separator: true,
            ..Self::action("", || {})
        }
    }
    pub fn enabled(mut self, value: bool) -> Self {
        self.enabled = value;
        self
    }
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
}
fn normalize(entries: Vec<Entry>) -> Vec<Entry> {
    let mut result: Vec<Entry> = Vec::new();
    for mut entry in entries {
        if entry.separator
            && (result.is_empty() || result.last().is_some_and(|item| item.separator))
        {
            continue;
        }
        entry.children = normalize(entry.children);
        result.push(entry);
    }
    if result.last().is_some_and(|item| item.separator) {
        result.pop();
    }
    result
}
#[derive(Clone, PartialEq)]
struct Request {
    id: u64,
    x: f64,
    y: f64,
    entries: Vec<Entry>,
}
#[derive(Clone)]
pub struct Menus {
    request: Signal<Option<Request>>,
    dialog: Signal<Option<Dialog>>,
    sequence: Rc<Cell<u64>>,
    #[cfg(target_os = "macos")]
    native: Rc<RefCell<Option<NativeMenu>>>,
}
#[cfg(target_os = "macos")]
struct NativeMenu {
    _menu: dioxus::desktop::muda::Menu,
    actions: HashMap<String, Rc<dyn Fn()>>,
}

pub fn use_menus() -> Menus {
    let menus = Menus {
        request: use_hook(|| Signal::new_in_scope(None, ScopeId::ROOT)),
        dialog: use_hook(|| Signal::new_in_scope(None, ScopeId::ROOT)),
        sequence: use_hook(|| Rc::new(Cell::new(0))),
        #[cfg(target_os = "macos")]
        native: use_hook(|| Rc::new(RefCell::new(None))),
    };
    use_context_provider(|| menus.clone());
    menus
}
impl Menus {
    pub fn close(&self) {
        let mut request = self.request;
        request.set(None);
    }
    pub fn dismiss(&self) {
        self.close();
        let mut dialog = self.dialog;
        dialog.set(None);
    }
    pub fn show(&self, x: f64, y: f64, entries: Vec<Entry>) {
        let entries = normalize(entries);
        if entries.is_empty() {
            return;
        }
        let id = self.sequence.get().wrapping_add(1);
        self.sequence.set(id);
        #[cfg(target_os = "macos")]
        if let Some(window) = try_consume_context::<dioxus::desktop::DesktopContext>() {
            use dioxus::desktop::{muda::ContextMenu, tao::platform::macos::WindowExtMacOS};
            let mut actions = HashMap::new();
            match native_menu(&entries, &format!("ytu-context-{id}"), &mut actions) {
                Ok(menu) => {
                    *self.native.borrow_mut() = Some(NativeMenu {
                        _menu: menu.clone(),
                        actions,
                    });
                    // SAFETY: Tao owns this live NSView and this handler runs on AppKit's main thread.
                    let selected = unsafe {
                        menu.show_context_menu_for_nsview(
                            window.window.ns_view(),
                            Some(dioxus::desktop::muda::dpi::LogicalPosition::new(x, y).into()),
                        )
                    };
                    if !selected {
                        self.native.borrow_mut().take();
                    }
                    return;
                }
                Err(error) => {
                    if let Some(state) = try_consume_context::<AppState>() {
                        state.notify(format!("No se pudo abrir el menú: {error}"));
                    }
                }
            }
        }
        let mut request = self.request;
        request.set(Some(Request { id, x, y, entries }));
    }
}
pub fn show(event: MouseEvent, entries: Vec<Entry>) {
    event.prevent_default();
    event.stop_propagation();
    let point = event.client_coordinates();
    if let Some(menus) = try_consume_context::<Menus>() {
        menus.show(point.x, point.y, entries);
    }
}

/// This adapter only exposes browser selection/editing primitives; menu policy
/// and clipboard operations stay in Rust. Values cross the bridge as JSON.
const EDIT_ADAPTER: &str = r#"
const q = await dioxus.recv();
if (q.op === 'inspect') {
    const el = q.active ? document.activeElement : document.elementFromPoint(q.x, q.y);
    let field = el?.closest('input,textarea,[contenteditable]:not([contenteditable="false"])');
    if (field?.tagName === 'INPUT' && !['text','search','url','email','tel','number','password'].includes(field.type)) field = null;
    const input = field && ['INPUT','TEXTAREA'].includes(field.tagName);
    const secret = field?.type === 'password';
    const selection = window.getSelection();
    const start = input ? field.selectionStart : null;
    const end = input ? field.selectionEnd : null;
    const text = secret ? '' : input && start !== null ? field.value.slice(start, end) : selection?.toString() || '';
    window.__ytuEditTarget = {token:q.token, field, start, end, range:selection?.rangeCount ? selection.getRangeAt(0).cloneRange() : null};
    return {field:!!field, selected:!!text, writable:!!field && !field.disabled && !field.readOnly, secret};
}
const target = window.__ytuEditTarget;
if (!target || target.token !== q.token || (target.field && !target.field.isConnected)) return null;
const field = target.field;
field?.focus({preventScroll:true});
if (field?.setSelectionRange && target.start !== null) { try {field.setSelectionRange(target.start, target.end);} catch (_) {} }
else if (target.range) {const s=window.getSelection();s.removeAllRanges();s.addRange(target.range);}
if (q.op === 'selection') {
    if (field?.type === 'password') return null;
    return field && target.start !== null ? field.value.slice(target.start, target.end) : window.getSelection()?.toString() || '';
}
if (q.op === 'selectAll') {if (field?.select) field.select(); else document.execCommand('selectAll');}
else if (q.op === 'undo' || q.op === 'redo') document.execCommand(q.op);
else if (field && !field.disabled && !field.readOnly) {
    if (q.op === 'paste') document.execCommand('insertText', false, q.text);
    else if (q.op === 'delete' && field.type !== 'password') document.execCommand('delete');
}
return null;
"#;

async fn edit_dom(value: serde_json::Value) -> Result<serde_json::Value, String> {
    let eval = document::eval(EDIT_ADAPTER);
    eval.send(value).map_err(|error| error.to_string())?;
    eval.await.map_err(|error| error.to_string())
}

async fn run_edit(token: u64, op: &str) -> Result<(), String> {
    match op {
        "copy" | "cut" => {
            let selection = edit_dom(serde_json::json!({"op":"selection","token":token})).await?;
            if let Some(text) = selection.as_str().filter(|text| !text.is_empty()) {
                crate::native::write_clipboard_text(text)?;
                if op == "cut" {
                    edit_dom(serde_json::json!({"op":"delete","token":token})).await?;
                }
            }
        }
        "paste" => {
            let text = crate::native::read_clipboard_text()?;
            edit_dom(serde_json::json!({"op":"paste","token":token,"text":text})).await?;
        }
        "selectAll" | "undo" | "redo" => {
            edit_dom(serde_json::json!({"op":op,"token":token})).await?;
        }
        _ => return Err("Acción de edición no válida".into()),
    }
    Ok(())
}

pub fn edit_current(state: &AppState, op: &str) {
    let Some(menus) = try_consume_context::<Menus>() else {
        return;
    };
    let token = menus.sequence.get().wrapping_add(1);
    menus.sequence.set(token);
    let state = state.clone();
    let op = op.to_owned();
    dioxus::core::spawn_forever(async move {
        let result = async {
            edit_dom(serde_json::json!({"op":"inspect","active":true,"token":token})).await?;
            run_edit(token, &op).await
        }
        .await;
        if let Err(error) = result {
            state.notify(format!("No se pudo editar el texto: {error}"));
        }
    });
}

pub fn show_edit(event: MouseEvent, state: AppState, menus: Menus) {
    event.prevent_default();
    let point = event.client_coordinates();
    let token = menus.sequence.get().wrapping_add(1);
    menus.sequence.set(token);
    dioxus::core::spawn_forever(async move {
        let Ok(target) =
            edit_dom(serde_json::json!({"op":"inspect","x":point.x,"y":point.y,"token":token}))
                .await
        else {
            return;
        };
        if menus.sequence.get() != token {
            return;
        }
        let field = target["field"].as_bool() == Some(true);
        let selected = target["selected"].as_bool() == Some(true);
        let writable = target["writable"].as_bool() == Some(true);
        let secret = target["secret"].as_bool() == Some(true);
        if !field && !selected {
            menus.close();
            return;
        }
        let action = |label: &str, op: &'static str, enabled| {
            let state = state.clone();
            Entry::action(label, move || {
                let state = state.clone();
                dioxus::core::spawn_forever(async move {
                    let result = run_edit(token, op).await;
                    if let Err(error) = result {
                        state.notify(format!("No se pudo editar el texto: {error}"));
                    }
                });
            })
            .enabled(enabled)
        };
        let items = if field {
            vec![
                action("Cortar", "cut", selected && writable && !secret),
                action("Copiar", "copy", selected && !secret),
                action("Pegar", "paste", writable),
                Entry::separator(),
                action("Seleccionar todo", "selectAll", true),
            ]
        } else {
            vec![action("Copiar", "copy", true)]
        };
        menus.show(point.x, point.y, items);
    });
}

pub fn use_document_controls(state: AppState, menus: Menus) -> Signal<bool> {
    let mut importing = use_signal(|| false);
    use_hook(move || {
        spawn(async move {
            let mut eval = document::eval(
                r#"
let depth=0;
document.addEventListener('keydown',e=>{
    if (e.defaultPrevented || e.key !== ' ' || e.ctrlKey || e.metaKey || e.altKey) return;
    if (e.target?.closest('input,textarea,select,[contenteditable]:not([contenteditable="false"])')) return;
    e.preventDefault(); if (!e.repeat) dioxus.send('play');
});
window.addEventListener('wheel',e=>{if (!e.target?.closest('.context-menu-layer')) dioxus.send('close');},{passive:true});
window.addEventListener('resize',()=>dioxus.send('close'));
window.addEventListener('blur',()=>{depth=0;dioxus.send('hide-drop');dioxus.send('close');});
window.addEventListener('dragenter',e=>{if(Array.from(e.dataTransfer?.types || []).includes('Files')) {e.preventDefault();depth++;dioxus.send('show-drop');}});
window.addEventListener('dragleave',e=>{if(Array.from(e.dataTransfer?.types || []).includes('Files')) {depth=Math.max(0,depth-1);if(!depth)dioxus.send('hide-drop');}});
window.addEventListener('drop',()=>{depth=0;dioxus.send('hide-drop');});
window.addEventListener('dragend',()=>{depth=0;dioxus.send('hide-drop');});
"#,
            );
            while let Ok(event) = eval.recv::<String>().await {
                match event.as_str() {
                    "play" => state.toggle_play(),
                    "close" => menus.close(),
                    "show-drop" => importing.set(true),
                    "hide-drop" => importing.set(false),
                    _ => {}
                }
            }
        })
    });
    importing
}
#[cfg(target_os = "macos")]
fn native_menu(
    entries: &[Entry],
    prefix: &str,
    actions: &mut HashMap<String, Rc<dyn Fn()>>,
) -> Result<dioxus::desktop::muda::Menu, dioxus::desktop::muda::Error> {
    use dioxus::desktop::muda::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
    fn append(
        entries: &[Entry],
        prefix: &str,
        actions: &mut HashMap<String, Rc<dyn Fn()>>,
        add: &dyn Fn(
            &dyn dioxus::desktop::muda::IsMenuItem,
        ) -> Result<(), dioxus::desktop::muda::Error>,
    ) -> Result<(), dioxus::desktop::muda::Error> {
        for (index, entry) in entries.iter().enumerate() {
            let id = format!("{prefix}-{index}");
            if entry.separator {
                add(&PredefinedMenuItem::separator())?;
            } else if !entry.children.is_empty() {
                let sub = Submenu::new(&entry.label, entry.enabled);
                append(&entry.children, &id, actions, &|item| sub.append(item))?;
                add(&sub)?;
            } else {
                if let Some(action) = &entry.action {
                    actions.insert(id.clone(), action.clone());
                }
                if let Some(checked) = entry.checked {
                    add(&CheckMenuItem::with_id(
                        id,
                        &entry.label,
                        entry.enabled,
                        checked,
                        None,
                    ))?;
                } else {
                    add(&MenuItem::with_id(id, &entry.label, entry.enabled, None))?;
                }
            }
        }
        Ok(())
    }
    let menu = Menu::new();
    append(entries, prefix, actions, &|item| menu.append(item))?;
    Ok(menu)
}
#[component]
pub fn ContextMenuHost() -> Element {
    let menus = use_context::<Menus>();
    #[cfg(target_os = "macos")]
    {
        let native = menus.native.clone();
        dioxus::desktop::use_muda_event_handler(move |event| {
            if event.id.0.starts_with("ytu-context-") {
                let action = native
                    .borrow_mut()
                    .take()
                    .and_then(|mut menu| menu.actions.remove(&event.id.0));
                if let Some(action) = action {
                    action();
                }
            }
        });
    }
    let close = menus.clone();
    let key_close = menus.clone();
    let request = menus.request.read().clone();
    let dialog = menus.dialog.read().clone();
    rsx! { if let Some(request)=request { div { class:"context-menu-layer",onmousedown:|event|event.prevent_default(),oncontextmenu:|event|{event.prevent_default();event.stop_propagation();},
        div { style:"position:fixed;inset:0;z-index:199", onmousedown:move |_|close.close(), oncontextmenu:|event|event.prevent_default() }
        div { onkeydown:move|event|if event.key()==Key::Escape{key_close.close();},
            MenuPanel { key:"{request.id}", entries:request.entries,x:request.x,y:request.y }
        }
    } }
        if let Some(dialog)=dialog { match dialog { Dialog::Delete(track)=>rsx!{DeleteDialog{track}},Dialog::Playlist(track)=>rsx!{PlaylistDialog{track}} } }
    }
}
#[component]
fn MenuPanel(
    entries: Vec<Entry>,
    x: f64,
    y: f64,
    #[props(default=None)] flip_x: Option<f64>,
) -> Element {
    let mut position = use_signal(|| (x, y));
    let mut submenu = use_signal(|| None::<(usize, f64, f64, f64)>);
    let checked = entries.iter().any(|item| item.checked.is_some());
    let menu = use_context::<Menus>();
    let child = submenu.read().as_ref().and_then(|(index, x, y, flip)| {
        entries
            .get(*index)
            .map(|entry| (entry.children.clone(), *x, *y, *flip))
    });
    rsx! {
        div{class:"context-menu",role:"menu",style:"left:{position.read().0}px;top:{position.read().1}px",onmounted:move|event|async move{
            if let Ok(rect)=event.get_client_rect().await {
                if let Some(window)=try_consume_context::<dioxus::desktop::DesktopContext>() {
                    let size=window.inner_size().to_logical::<f64>(window.scale_factor());
                    let left=if x+rect.width()>size.width-6.0 {flip_x.map_or(size.width-rect.width()-6.0,|flip|flip-rect.width())}else{x};
                    position.set((left.max(6.0),y.min(size.height-rect.height()-6.0).max(6.0)));
                }
            }
        },
            for(index,entry)in entries.into_iter().enumerate(){
                if entry.separator {div{class:"context-menu-separator",role:"separator"}}
                else { MenuButton{ entry,checked,open:submenu.read().as_ref().is_some_and(|value|value.0==index),onhover:move|point:Option<(f64,f64,f64)>|submenu.set(point.map(|(x,y,flip)|(index,x,y,flip))),onchoose:{let menu=menu.clone();move |_|menu.close()} } }
            }
        }
        if let Some((entries,x,y,flip))=child {MenuPanel{entries,x,y,flip_x:Some(flip)}}
    }
}
#[component]
fn MenuButton(
    entry: Entry,
    checked: bool,
    open: bool,
    onhover: EventHandler<Option<(f64, f64, f64)>>,
    onchoose: EventHandler<()>,
) -> Element {
    let mut mounted = use_signal(|| None::<Rc<MountedData>>);
    let has_submenu = !entry.children.is_empty();
    let enabled = entry.enabled;
    let hover = move |_| {
        if !has_submenu || !enabled {
            onhover.call(None);
            return;
        }
        let element = mounted.peek().clone();
        spawn(async move {
            if let Some(element) = element {
                if let Ok(rect) = element.get_client_rect().await {
                    onhover.call(Some((
                        rect.max_x() - 2.0,
                        rect.min_y() - 5.0,
                        rect.min_x() + 2.0,
                    )));
                }
            }
        });
    };
    let hover_click = hover;
    rsx! {button{r#type:"button",tabindex:"-1",role:if entry.checked.is_some(){"menuitemcheckbox"}else{"menuitem"},aria_checked:entry.checked.map(|value|value.to_string()),aria_disabled:(!entry.enabled).to_string(),aria_haspopup:if has_submenu{"menu"}else{"false"},aria_expanded:open.to_string(),
        class:format!("context-menu-item{}{}",if entry.danger{" danger"}else{""},if open{" context-menu-item--open"}else{""}),
        onmounted:move|event|mounted.set(Some(event.data())),onmouseenter:hover,
        onclick:move|event|{event.stop_propagation();if !entry.enabled{return;}
            if has_submenu {hover_click(event);return;}
            onchoose.call(());if let Some(action)=&entry.action{action();}},
        if checked{span{class:"context-menu-check",if entry.checked==Some(true){Icon{name:"check",size:13}}}}
        span{class:"context-menu-label-text","{entry.label}"}
        if has_submenu{span{class:"context-menu-chevron","›"}}
    }}
}

pub fn show_track(event: MouseEvent, state: AppState, track: Track) {
    show(event, track_items(state, track));
}

pub fn home_items(state: AppState, track: Track) -> Vec<Entry> {
    let local = !crate::remote::is_remote(&track);
    normalize(
        track_items(state, track)
            .into_iter()
            .filter(|entry| {
                entry.separator
                    || matches!(
                        entry.label.as_str(),
                        "Reproducir"
                            | "Reproducir a continuación"
                            | "Agregar a la cola"
                            | "Mostrar en Finder"
                            | "Mostrar en Explorador"
                            | "Abrir con la app predeterminada"
                            | "Copiar ruta"
                    )
                    || (local && entry.label == "Versión Alvin y las ardillas…")
            })
            .collect(),
    )
}
#[derive(Clone)]
enum Dialog {
    Delete(Track),
    Playlist(Track),
}

pub fn track_items(state: AppState, track: Track) -> Vec<Entry> {
    let local = !track.path.as_os_str().is_empty() && !track.missing;
    let metadata = state
        .services
        .core
        .track_metadata(&track.id)
        .unwrap_or_default();
    let advanced = state.settings.peek().show_advanced_tools;
    let play = state.clone();
    let play_track = track.clone();
    let next = state.clone();
    let next_track = track.clone();
    let later = state.clone();
    let later_track = track.clone();
    let mut entries = vec![
        Entry::action("Reproducir", move || {
            if play
                .current
                .peek()
                .as_ref()
                .is_some_and(|current| current.id == play_track.id)
            {
                play.toggle_play();
            } else {
                play.play(play_track.clone());
            }
        }),
        Entry::action("Reproducir a continuación", move || {
            let mut queue = next.queue;
            queue.write().insert(0, next_track.clone());
            next.persist_playback();
            next.notify("Se reproducirá a continuación");
        }),
        Entry::action("Agregar a la cola", move || {
            let mut queue = later.queue;
            queue.write().push(later_track.clone());
            later.persist_playback();
            later.notify("Agregado a la cola");
        }),
        Entry::separator(),
    ];
    let mut playlists = Vec::new();
    for playlist in state.playlists.peek().iter().cloned() {
        let state = state.clone();
        let id = track.id.clone();
        let name = playlist.name.clone();
        playlists.push(
            Entry::action(name, move || {
                match state.services.core.add_to_playlist(&playlist.id, &id) {
                    Ok(_) => {
                        state.refresh();
                        state.notify(format!("Añadido a \"{}\"", playlist.name));
                    }
                    Err(error) => state.notify(error.to_string()),
                }
            })
            .enabled(local),
        );
    }
    if playlists.is_empty() {
        playlists.push(Entry::action("Sin playlists", || {}).enabled(false));
    }
    playlists.push(Entry::separator());
    let menus = try_consume_context::<Menus>();
    let create_track = track.clone();
    let create_menus = menus.clone();
    playlists.push(
        Entry::action("Nueva playlist…", move || {
            if let Some(menus) = &create_menus {
                let mut dialog = menus.dialog;
                dialog.set(Some(Dialog::Playlist(create_track.clone())));
            }
        })
        .enabled(local),
    );
    entries.push(Entry::submenu("Añadir a playlist", playlists));
    entries.push(Entry::separator());
    if advanced {
        let separate = state.clone();
        let id = track.id.clone();
        entries.push(
            Entry::action("Separar vocales e instrumental", move || {
                crate::processing::start_processing(
                    separate.clone(),
                    id.clone(),
                    crate::processing::ProcessingTask::StemsFormat(yt_audio::AudioFormat::Flac),
                );
            })
            .enabled(
                local && cfg!(feature = "native-stems") && metadata["playlist"] != "Separaciones",
            ),
        );
        let mut variants = Vec::new();
        for (label, strong) in [("Intenso · FLAC", true), ("Equilibrado · FLAC", false)] {
            let state = state.clone();
            let id = track.id.clone();
            variants.push(Entry::action(label, move || {
                crate::processing::start_processing(
                    state.clone(),
                    id.clone(),
                    crate::processing::ProcessingTask::Variant(
                        yt_audio::VariantOptions::variation(&id, strong),
                        if strong { "Intenso" } else { "Equilibrado" },
                    ),
                );
            }));
        }
        let quality = state.clone();
        let id = track.id.clone();
        variants.push(
            Entry::action("CALIDAD · FLAC", move || {
                crate::processing::start_processing(
                    quality.clone(),
                    id.clone(),
                    crate::processing::ProcessingTask::Quality,
                );
            })
            .enabled(cfg!(feature = "native-stems")),
        );
        entries.push(
            Entry::submenu("Modificar fingerprint", variants).enabled(
                local && track.media_type == "audio" && metadata["playlist"] != "Protegidos",
            ),
        );
    }
    let chipmunk = state.clone();
    let chipmunk_track = track.clone();
    entries.push(
        Entry::action("Versión Alvin y las ardillas…", move || {
            let mut signal = chipmunk.chipmunk_track;
            signal.set(Some(chipmunk_track.clone()));
        })
        .enabled(local),
    );
    entries.push(Entry::separator());
    let info = state.clone();
    let info_track = track.clone();
    entries.push(Entry::action("Obtener información", move || {
        let mut signal = info.information_track;
        signal.set(Some(info_track.clone()));
    }));
    let reveal = state.clone();
    let reveal_path = track.path.clone();
    entries.push(
        Entry::action(
            if cfg!(target_os = "macos") {
                "Mostrar en Finder"
            } else {
                "Mostrar en Explorador"
            },
            move || crate::native::reveal_file(&reveal, &reveal_path),
        )
        .enabled(local),
    );
    let open = state.clone();
    let open_path = track.path.clone();
    entries.push(
        Entry::action("Abrir con la app predeterminada", move || {
            crate::native::open_default_file(&open, &open_path)
        })
        .enabled(local),
    );
    let copy = state.clone();
    let copy_path = track.path.to_string_lossy().into_owned();
    entries.push(
        Entry::action("Copiar ruta", move || {
            crate::native::copy_text(&copy, &copy_path)
        })
        .enabled(local),
    );
    entries.push(Entry::separator());
    entries.push(
        Entry::action("Eliminar de la biblioteca…", move || {
            if let Some(menus) = &menus {
                let mut dialog = menus.dialog;
                dialog.set(Some(Dialog::Delete(track.clone())));
            }
        })
        .danger()
        .enabled(local),
    );
    entries
}

#[component]
fn DeleteDialog(track: Track) -> Element {
    let state = use_context::<AppState>();
    let menus = use_context::<Menus>();
    let close = menus.clone();
    let cancel = menus.clone();
    let done = menus.clone();
    rsx! {div{class:"modal-backdrop",onclick:move |_|close.dismiss(),div{class:"modal-panel",role:"alertdialog",aria_label:"Eliminar archivo",onclick:|event|event.stop_propagation(),
        div{class:"modal-header",div{class:"modal-title","¿Eliminar este archivo permanentemente?"}}
        div{class:"modal-body",p{"{track.title}"}p{class:"rust-settings-note","{track.path.display()}"}}
        div{class:"modal-actions",button{class:"btn-secondary",onclick:move |_|cancel.dismiss(),"Cancelar"}
            button{class:"btn-accent rust-danger",onclick:move |_|{let core=state.services.core.clone();let id=track.id.clone();let state=state.clone();done.dismiss();dioxus::core::spawn_forever(async move{
                match tokio::task::spawn_blocking(move||core.delete_track_file(&id)).await{Ok(Ok(()))=>{state.refresh();state.notify("Archivo eliminado");},Ok(Err(error))=>state.notify(format!("No se pudo eliminar el archivo: {error}")),Err(error)=>state.notify(error.to_string())}
            });},"Eliminar"}
        }
    }}}
}
#[component]
fn PlaylistDialog(track: Track) -> Element {
    let state = use_context::<AppState>();
    let menus = use_context::<Menus>();
    let close = menus.clone();
    let cancel = menus.clone();
    let mut name = use_signal(String::new);
    rsx! {div{class:"modal-backdrop",onclick:move |_|close.dismiss(),form{class:"modal-panel",role:"dialog",aria_label:"Nueva playlist",onclick:|event|event.stop_propagation(),onsubmit:move|event|{event.prevent_default();let value=name.peek().trim().to_owned();if value.is_empty(){return;}let result=state.services.core.create_playlist(&value).and_then(|playlist|state.services.core.add_to_playlist(&playlist.id,&track.id));state.refresh();match result{Ok(_)=>{menus.dismiss();state.notify(format!("Playlist \"{value}\" creada"));},Err(error)=>state.notify(error.to_string())}},
        div{class:"modal-header",div{class:"modal-title","Nombre de la nueva playlist:"}}
        div{class:"modal-body",input{class:"input-search",autofocus:true,aria_label:"Nombre de la nueva playlist",value:"{name}",oninput:move|event|name.set(event.value())}}
        div{class:"modal-actions",button{r#type:"button",class:"btn-secondary",onclick:move |_|cancel.dismiss(),"Cancelar"}button{r#type:"submit",class:"btn-accent",disabled:name.read().trim().is_empty(),"Crear"}}
    }}}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{use_app_state, Services};
    use std::cell::RefCell;
    type Capture = Rc<RefCell<Option<AppState>>>;
    fn harness() -> Element {
        let state = use_app_state(use_context::<Rc<Services>>());
        *use_context::<Capture>().borrow_mut() = Some(state.clone());
        use_context_provider(|| state);
        use_menus();
        rsx! {ContextMenuHost{}}
    }
    #[tokio::test]
    async fn original_page_menus_preserve_queue_actions_and_processing_guards() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let track = services
            .core
            .import_bytes("original.wav", &[1, 2, 3])
            .unwrap();
        let capture: Capture = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(harness);
        dom.insert_any_root_context(Box::new(services.clone()));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let mut state = capture.borrow().as_ref().unwrap().clone();
        dom.in_scope(ScopeId::ROOT, || {
            let home = home_items(state.clone(), track.clone());
            let labels = home
                .iter()
                .filter(|entry| !entry.separator)
                .map(|entry| entry.label.as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                &labels[..4],
                [
                    "Reproducir",
                    "Reproducir a continuación",
                    "Agregar a la cola",
                    "Versión Alvin y las ardillas…"
                ]
            );
            assert_eq!(labels.len(), 7);
            home[1].action.as_ref().unwrap()();
            home[2].action.as_ref().unwrap()();
            assert_eq!(
                services.core.load_playback().unwrap().queue,
                vec![track.id.clone(), track.id.clone()]
            );
            assert!(!track_items(state.clone(), track.clone())
                .iter()
                .any(|entry| entry.label == "Modificar fingerprint"));
            state.settings.write().show_advanced_tools = true;
            let enabled = track_items(state.clone(), track.clone());
            assert!(
                enabled
                    .iter()
                    .find(|entry| entry.label == "Modificar fingerprint")
                    .unwrap()
                    .enabled
            );
            services
                .core
                .merge_track_metadata(&track.id, &serde_json::json!({"playlist":"Protegidos"}))
                .unwrap();
            let protected = track_items(state.clone(), track.clone());
            assert!(
                !protected
                    .iter()
                    .find(|entry| entry.label == "Modificar fingerprint")
                    .unwrap()
                    .enabled
            );
            assert!(
                protected
                    .iter()
                    .find(|entry| entry.label == "Eliminar de la biblioteca…")
                    .unwrap()
                    .danger
            );
            assert!(track.path.exists());
        });
    }
}
