use crate::{
    icons::Icon,
    state::{size_label, AppState, Page},
};
use dioxus::prelude::*;
use std::{collections::BTreeMap, path::PathBuf, rc::Rc};
use yt_media::{CancellationToken, StreamKind};

#[component]
pub fn SettingsPage() -> Element {
    let state = use_context::<AppState>();
    let version = env!("CARGO_PKG_VERSION");
    let settings = state.settings.read().clone();
    let tracks = state.library.read().clone();
    let audio_count = tracks.iter().filter(|t| t.media_type == "audio").count();
    let total_size = tracks.iter().map(|t| t.size_bytes).sum();
    let mut mounts = use_signal(BTreeMap::<String, Rc<MountedData>>::new);
    let section = state.settings_section.read().clone();
    let navigation = state.clone();
    use_effect(move || {
        let selected = navigation.settings_section.read().clone();
        let selected = if selected == "plugin-scaffold" {
            "plugins"
        } else {
            selected.as_str()
        };
        if let Some(element) = mounts.read().get(selected).cloned() {
            spawn(async move {
                let _ = element.scroll_to(ScrollBehavior::Smooth).await;
            });
        }
    });
    let open = state.clone();
    let clean = state.clone();
    let scan = state.clone();
    rsx! {div{class:"page-shell page-shell--narrow",
      div{class:"page-header",h1{class:"page-title","Ajustes"}div{class:"page-subtitle","Los cambios se guardan automáticamente"}}
      section{class:"settings-section",
        SectionHeader{icon:"music",title:"Calidad de audio",description:"Para nuevas descargas MP3"}
        for (value,description) in [("128","Ahorro de espacio"),("192","Equilibrado"),("256","Alta calidad"),("320","Máxima calidad")] {
          Choice{active:settings.audio_quality==value,label:format!("{value} kbps"),description,key_name:"audio_quality",value}
        }
      }
      section{class:"settings-section",
        SectionHeader{icon:"video",title:"Calidad de vídeo",description:"Para nuevas descargas MP4"}
        for (value,label,description) in [("best","Máxima disponible","Usa la mejor fuente compatible"),("2160","2160p","4K"),("1080","1080p","Full HD"),("720","720p","Ahorro de espacio")] {
          Choice{active:settings.video_quality==value,label,description,key_name:"video_quality",value}
        }
        if !["best","2160","1080","720"].contains(&settings.video_quality.as_str()) {
          div{class:"settings-row",span{class:"settings-row__label",span{class:"settings-row__title","{settings.video_quality}p"}span{class:"settings-row__sub","Preferencia guardada"}}span{class:"settings-row__check active",Icon{name:"check",size:11}}}
        }
      }
      section{class:"settings-section",
        SectionHeader{icon:"hard-drive",title:"Biblioteca",description:state.services.library_dir.display().to_string()}
        div{class:"settings-stat-grid",
          Stat{label:"Archivos",value:tracks.len().to_string()}
          Stat{label:"Canciones",value:audio_count.to_string()}
          Stat{label:"Espacio",value:size_label(total_size)}
        }
        button{class:"settings-row",onclick:move |_|crate::native::open_folder(&open,&open.services.library_dir),span{class:"settings-row__main",Icon{name:"folder",size:15}span{class:"settings-row__label",span{class:"settings-row__title",if cfg!(target_os="macos"){"Abrir en Finder"}else{"Abrir en Explorador"}}}}Icon{name:"chevron-right",size:13}}
        button{class:"settings-row",onclick:move |_|clean.clean_temporary_files(),span{class:"settings-row__main",Icon{name:"trash",size:15}span{class:"settings-row__label",span{class:"settings-row__title","Limpiar temporales"}span{class:"settings-row__sub","Elimina temporales obsoletos sin afectar a las descargas guardadas"}}}Icon{name:"chevron-right",size:13}}
        button{class:"settings-row",onclick:move |_|{let state=scan.clone();dioxus::core::spawn_forever(async move{let core=state.services.core.clone();match tokio::task::spawn_blocking(move||core.scan_library()).await{Ok(Ok(_))=>{state.refresh();state.notify("Biblioteca actualizada");},Ok(Err(e))=>state.notify(e.to_string()),Err(e)=>state.notify(e.to_string())}});},span{class:"settings-row__title","Actualizar biblioteca"}Icon{name:"chevron-right",size:13}}
      }
      div{id:"connection",onmounted:move|event|{mounts.write().insert("connection".into(),event.data());},ConnectionPanel{}}
      details{class:"settings-disclosure",open:settings.developer_mode,
        summary{span{Icon{name:"wrench",size:15}"Opciones avanzadas"}Icon{name:"chevron-right",size:14}}
        div{class:"settings-disclosure__content",
          Toggle{key_name:"show_advanced_tools",title:"Herramientas avanzadas",description:"Muestra Stems, fingerprint y controles técnicos",checked:settings.show_advanced_tools}
          Toggle{key_name:"developer_mode",title:"Modo desarrollador",description:"Activa plugins locales, logs, diagnósticos y el menú Desarrollo",checked:settings.developer_mode,tone:"warning"}
          if settings.show_advanced_tools {AdvancedOptions{}}
        }
      }
      if settings.developer_mode {
        div{id:"plugins",onmounted:move|event|{mounts.write().insert("plugins".into(),event.data());},crate::plugins::PluginPanel{}}
      }
      if settings.developer_mode || section=="diagnostics" {
        div{id:"diagnostics",onmounted:move|event|{mounts.write().insert("diagnostics".into(),event.data());},DiagnosticsPanel{}}
      }
      div{id:"updates",onmounted:move|event|{mounts.write().insert("updates".into(),event.data());},crate::updates::UpdatePanel{}}
      div{class:"settings-footer","YT Ultimate {version}"}
    }}
}

#[component]
fn SectionHeader(icon: String, title: String, description: String) -> Element {
    rsx! {div{class:"settings-section__header",div{class:"settings-section__icon",Icon{name:icon,size:15}}div{div{class:"settings-section__title","{title}"}div{class:"settings-section__sub","{description}"}}}}
}
#[component]
fn Stat(label: String, value: String) -> Element {
    rsx! {div{class:"settings-stat",div{class:"settings-stat__value","{value}"}div{class:"settings-stat__label","{label}"}}}
}
#[component]
fn Choice(
    active: bool,
    label: String,
    description: String,
    key_name: &'static str,
    value: &'static str,
) -> Element {
    let state = use_context::<AppState>();
    rsx! {button{r#type:"button",class:"settings-row",onclick:move |_|state.update_setting(key_name,serde_json::json!(value)),span{class:"settings-row__label",span{class:"settings-row__title","{label}"}span{class:"settings-row__sub","{description}"}}span{class:if active{"settings-row__check active"}else{"settings-row__check"},Icon{name:"check",size:11}}}}
}
#[component]
fn Toggle(
    key_name: &'static str,
    title: String,
    description: String,
    checked: bool,
    #[props(default="default".into())] tone: String,
) -> Element {
    let state = use_context::<AppState>();
    rsx! {button{r#type:"button",class:"settings-row settings-row--toggle settings-row--{tone}",role:"switch",aria_checked:checked,onclick:move |_|state.update_setting(key_name,serde_json::json!(!checked)),span{class:"settings-row__label",span{class:"settings-row__title","{title}"}span{class:"settings-row__sub","{description}"}}span{class:if checked{"settings-switch active"}else{"settings-switch"},span{}}}}
}
#[component]
fn NumberSetting(
    name: &'static str,
    label: &'static str,
    value: u64,
    min: u64,
    max: u64,
) -> Element {
    let state = use_context::<AppState>();
    let mut draft = use_signal(|| value.to_string());
    use_effect(move || draft.set(value.to_string()));
    rsx! {label{span{"{label}"}input{r#type:"number",aria_label:label,value:"{draft}",min:"{min}",max:"{max}",oninput:move|event|draft.set(event.value()),onblur:move |_|{let parsed=draft.peek().parse::<u64>();match parsed{Ok(v) if (min..=max).contains(&v)=>state.update_setting(name,serde_json::json!(v)),_=>{draft.set(value.to_string());state.notify(format!("{label}: escribe un valor entre {min} y {max}"));}}}}}}
}

#[component]
fn AdvancedOptions() -> Element {
    let state = use_context::<AppState>();
    let settings = state.settings.read().clone();
    let mut template = use_signal(|| settings.filename_template.clone());
    rsx! {
      div{class:"advanced-grid",
        NumberSetting{name:"max_concurrent_downloads",label:"Descargas simultáneas",value:settings.max_concurrent_downloads as u64,min:1,max:8}
        NumberSetting{name:"max_concurrent_processing",label:"Procesos pesados",value:settings.max_concurrent_processing as u64,min:1,max:4}
        label{class:"advanced-grid__wide",span{"Plantilla de archivo"}input{r#type:"text",aria_label:"Plantilla de archivo",value:"{template}",oninput:move|event|template.set(event.value()),onblur:{let state=state.clone();move |_|{let value=template.peek().trim().to_owned();let value=if value.is_empty(){"{title}".into()}else{value};match crate::downloads::validate_filename_template(&value){Ok(())=>state.update_setting("filename_template",serde_json::json!(value)),Err(error)=>state.notify(error)}}}}small{{"{title} · {channel} · {video_id} · {media_type} · {quality}"}}}
        label{span{"Descarga predeterminada"}select{value:"{settings.default_download_type}",aria_label:"Descarga predeterminada",onchange:{let state=state.clone();move|event|state.update_setting("default_download_type",serde_json::json!(event.value()))},option{value:"audio","Audio MP3"}option{value:"video","Vídeo MP4"}}}
        label{span{"Idioma de subtítulos"}select{value:"{settings.language}",aria_label:"Idioma de subtítulos",onchange:{let state=state.clone();move|event|state.update_setting("language",serde_json::json!(event.value()))},option{value:"es","Español"}option{value:"en","English"}}}
      }
      Toggle{key_name:"auto_thumbnail",title:"Guardar carátulas",description:"Incluye la imagen del vídeo en las nuevas descargas",checked:settings.auto_thumbnail}
      Toggle{key_name:"preserve_source_metadata",title:"Conservar metadatos",description:"Mantiene títulos, artistas y carátulas al descargar y exportar audio",checked:settings.preserve_source_metadata}
      Toggle{key_name:"download_subtitles",title:"Descargar subtítulos",description:"Guarda los subtítulos disponibles junto al archivo",checked:settings.download_subtitles}
      div{class:"advanced-grid",label{class:"advanced-grid__wide",span{"Importación"}select{aria_label:"Importación",value:"{settings.import_mode}",onchange:{let state=state.clone();move|event|state.update_setting("import_mode",serde_json::json!(event.value()))},option{value:"copy","Copiar a la biblioteca"}option{value:"reference","Usar ubicación original"}}}}
      Toggle{key_name:"scan_library_on_startup",title:"Buscar archivos al iniciar",description:"Añade nuevos archivos de la carpeta de biblioteca",checked:settings.scan_library_on_startup}
    }
}

#[component]
fn ConnectionPanel() -> Element {
    let state = use_context::<AppState>();
    let settings = state.settings.read().clone();
    let mut browser = use_signal(|| {
        settings
            .extra
            .get("youtube_cookies_browser")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    });
    let mut profile = use_signal(|| {
        settings
            .extra
            .get("youtube_browser_profile")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    });
    let mut proxy = use_signal(|| {
        settings
            .extra
            .get("youtube_proxy")
            .or_else(|| settings.extra.get("proxy"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    });
    let cookie_file = settings
        .extra
        .get("youtube_cookie_file")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut busy = use_signal(|| false);
    let mut connection = use_signal(String::new);
    let mut revision = use_signal(|| 0u64);
    let mut diagnostic = use_signal(String::new);
    let mut testing = use_signal(|| None::<CancellationToken>);
    let active_session = state.services.browser_session_info();
    let connect_state = state.clone();
    let disconnect_state = state.clone();
    let file_state = state.clone();
    let remove_state = state.clone();
    let test_state = state.clone();
    rsx! {section{class:"settings-section",
      SectionHeader{icon:"activity",title:"Conexión con YouTube",description:"Si YouTube pide iniciar sesión, elige las cookies de tu navegador o un archivo Netscape."}
      div{class:"advanced-grid",
        label{class:"advanced-grid__wide",span{"Cookies del navegador"}select{aria_label:"Cookies del navegador",value:"{browser}",disabled:*busy.read(),onchange:move|event|browser.set(event.value()),option{value:"","Sin navegador"}for name in ["chrome","chromium","edge","firefox","safari","brave","opera","vivaldi","whale"]{option{value:name,disabled:name=="safari"&&!cfg!(target_os="macos"),"{name}"}}}}
        label{class:"advanced-grid__wide",span{"Perfil del navegador"}input{r#type:"text",aria_label:"Perfil del navegador",placeholder:"Predeterminado",value:"{profile}",readonly:true}}
        button{class:"btn-secondary",disabled:*busy.read(),onclick:move |_|{spawn(async move{if let Some(folder)=rfd::AsyncFileDialog::new().set_title("Elegir perfil del navegador").pick_folder().await{profile.set(folder.path().to_string_lossy().into_owned());}});},Icon{name:"folder",size:13}"Elegir perfil"}
        button{class:"btn-secondary",disabled:*busy.read()||profile.read().is_empty(),onclick:move |_|profile.set(String::new()),"Perfil predeterminado"}
        button{class:"btn-secondary",disabled:*busy.read()||browser.read().is_empty(),onclick:move |_|{
          let state=connect_state.clone();let selected=browser.peek().clone();let path=profile.peek().clone();let request=*revision.peek()+1;revision.set(request);busy.set(true);connection.set("Esperando autorización del sistema…".into());
          spawn(async move{
            let source=selected.clone();let source_path=path.clone();
            let result=tokio::task::spawn_blocking(move||yt_media::read_browser_cookies(&source,if source_path.is_empty(){None}else{Some(std::path::Path::new(&source_path))})).await;
            if *revision.peek()!=request{return;}busy.set(false);
            match result {Ok(Ok(session))=>{
              let mut values=state.settings.peek().clone();values.extra.insert("youtube_cookie_file".into(),serde_json::json!(""));values.extra.insert("youtube_cookies_browser".into(),serde_json::json!(selected));values.extra.insert("youtube_browser_profile".into(),serde_json::json!(path));state.save_settings(values);
              match state.services.set_browser_session(session){Ok(())=>connection.set("Conectado durante esta sesión. Las cookies no se guardan en disco.".into()),Err(error)=>connection.set(error)}
            },Ok(Err(error))=>connection.set(error.to_string()),Err(_)=>connection.set("La conexión con el navegador se interrumpió.".into())}
          });
        },if *busy.read(){"Conectando…"}else{"Conectar"}}
        if *busy.read(){button{class:"btn-secondary",onclick:move |_|{revision+=1;busy.set(false);connection.set("Conexión cancelada; no se usará su resultado.".into());},"Cancelar"}}
        if active_session.is_some(){button{class:"btn-secondary",onclick:move |_|{match disconnect_state.services.clear_browser_session(){Ok(())=>{disconnect_state.update_setting("youtube_cookies_browser",serde_json::json!(""));connection.set("Sesión del navegador desconectada.".into());},Err(error)=>connection.set(error)}},"Desconectar navegador"}}
        if let Some((name,count))=active_session{p{class:"advanced-grid__wide settings-row__sub","{name}: {count} cookies de YouTube autorizadas"}}
        if !connection.read().is_empty(){p{class:"advanced-grid__wide settings-row__sub",role:"status","{connection}"}}
        label{class:"advanced-grid__wide",span{"Archivo de cookies"}input{r#type:"text",aria_label:"Archivo de cookies",value:"{cookie_file}",readonly:true,placeholder:"Ruta al archivo cookies.txt"}}
        button{class:"btn-secondary",onclick:move |_|{let state=file_state.clone();spawn(async move{if let Some(file)=rfd::AsyncFileDialog::new().set_title("Elegir archivo de cookies Netscape").add_filter("Cookies",&["txt"]).pick_file().await{if let Err(error)=state.services.clear_browser_session(){state.notify(error);return;}let mut values=state.settings.peek().clone();values.extra.insert("youtube_cookie_file".into(),serde_json::json!(file.path().to_string_lossy()));values.extra.insert("youtube_cookies_browser".into(),serde_json::json!(""));state.save_settings(values);browser.set(String::new());connection.set(String::new());}});},Icon{name:"folder",size:13}"Elegir archivo"}
        button{class:"btn-secondary",disabled:cookie_file.is_empty(),onclick:move |_|remove_state.update_setting("youtube_cookie_file",serde_json::json!("")),"Quitar cookies"}
        NumberSetting{name:"download_retries",label:"Reintentos de descarga",value:settings.download_retries as u64,min:0,max:10}
        NumberSetting{name:"download_socket_timeout",label:"Espera de red (segundos)",value:settings.download_socket_timeout as u64,min:1,max:120}
        label{class:"advanced-grid__wide",span{"Proxy HTTP(S)"}input{r#type:"password",aria_label:"Proxy HTTP(S)",placeholder:"Sin proxy",value:"{proxy}",oninput:move|event|proxy.set(event.value()),onblur:{let state=state.clone();move |_|state.update_setting("youtube_proxy",serde_json::json!(proxy.peek().trim().to_owned()))}}}
      }
      div{class:"advanced-grid",
        button{class:"btn-secondary",disabled:testing.read().is_some(),onclick:move |_|{
          let state=test_state.clone();let cancel=CancellationToken::new();testing.set(Some(cancel.clone()));diagnostic.set("Comprobando búsqueda y acceso al audio…".into());
          spawn(async move{
            let result=async{let youtube=state.services.youtube_client().map_err(|e|e.to_string())?;tokio::select!{_ = cancel.cancelled()=>Err("Comprobación cancelada".to_owned()),result=async{let results=youtube.search("Big Buck Bunny Blender",1).await.map_err(|e|e.to_string())?;if results.is_empty(){return Err("La búsqueda no devolvió resultados".into());}youtube.resolve("aqz-KE-bpKQ",StreamKind::Audio).await.map_err(|e|e.to_string())?;Ok("Conexión disponible: búsqueda y fuente de audio verificadas.".to_owned())}=>result}}.await;
            diagnostic.set(result.unwrap_or_else(|e|format!("No se pudo verificar la conexión: {e}")));testing.set(None);
          });
        },"Probar conexión"}
        if let Some(token)=testing.read().clone(){button{class:"btn-secondary",onclick:move |_|token.cancel(),"Cancelar comprobación"}}
        if !diagnostic.read().is_empty(){p{class:"advanced-grid__wide settings-row__sub",role:"status","{diagnostic}"}}
      }
    }}
}

fn model_available() -> bool {
    #[cfg(feature = "native-stems")]
    {
        crate::processing::model_path().is_ok_and(|path| path.is_file())
    }
    #[cfg(not(feature = "native-stems"))]
    {
        false
    }
}
fn diagnostic_snapshot(state: &AppState) -> serde_json::Value {
    let jobs = state.services.core.jobs().unwrap_or_default();
    serde_json::json!({"application":"YT Ultimate","version":env!("CARGO_PKG_VERSION"),"platform":std::env::consts::OS,
      "generated_at":chrono::Utc::now().to_rfc3339(),"engines":{"audio_output":state.services.audio.is_ok(),"youtube_resolver":true,"audio_conversion":true,"video_mux":true,"separator_model":model_available()},
      "library":{"tracks":state.library.peek().len(),"playlists":state.playlists.peek().len()},"jobs":{"active":jobs.iter().filter(|j|!j.status.is_terminal()).count(),"errors":jobs.iter().filter(|j|j.status==yt_core::JobStatus::Error).count()},"connection":{"browser_session":state.services.browser_session_info().is_some(),"cookie_file_configured":state.settings.peek().extra.get("youtube_cookie_file").and_then(serde_json::Value::as_str).is_some_and(|v|!v.is_empty())}})
}
fn write_diagnostics(
    profile: &std::path::Path,
    value: &serde_json::Value,
) -> Result<PathBuf, String> {
    use std::io::Write;
    let folder = profile.join("logs");
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let path = folder.join("native-diagnostics.json");
    let mut temporary = tempfile::NamedTempFile::new_in(&folder).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    temporary.write_all(&bytes).map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary.persist(&path).map_err(|e| e.error.to_string())?;
    Ok(path)
}
#[component]
fn DiagnosticsPanel() -> Element {
    let state = use_context::<AppState>();
    let mut jobs = use_signal(|| state.services.core.jobs().unwrap_or_default());
    let polling = state.clone();
    use_hook(move || {
        spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                if *polling.page.peek() == Page::Settings {
                    if let Ok(value) = polling.services.core.jobs() {
                        jobs.set(value);
                    }
                }
            }
        });
    });
    let rows = jobs
        .read()
        .iter()
        .filter(|job| !job.status.is_terminal())
        .cloned()
        .collect::<Vec<_>>();
    let model = model_available();
    let logs = state.clone();
    let refresh = state.clone();
    rsx! {section{class:"settings-section",
      SectionHeader{icon:"activity",title:"Diagnóstico",description:format!("YT Ultimate {} · {}",env!("CARGO_PKG_VERSION"),std::env::consts::OS)}
      div{class:"engine-grid",for(name,available)in[("Reproducción de audio",state.services.audio.is_ok()),("Conexión YouTube",state.services.youtube_client().is_ok()),("Conversión de audio",true),("Vídeo MP4",true),("Separador",model)]{div{class:"engine-row",Icon{name:"gauge",size:13}span{"{name}"}span{class:if available{"engine-ok"}else{"engine-missing"},if available{"Disponible"}else{"No disponible"}}}}}
      if !model {p{class:"settings-row__sub",if cfg!(feature="native-stems"){"No se encontró el modelo de separación. Reinstala la edición Full para restaurarlo."}else{"Esta edición no incluye el separador. Está disponible en la edición Full."}}}
      if !rows.is_empty(){div{class:"settings-empty","Procesos en curso"}for job in rows{div{class:"settings-row",span{class:"settings-row__label",span{class:"settings-row__title","{job.kind}"}span{class:"settings-row__sub","{job.message} · {job.progress.round()}%"}}button{class:"btn-secondary",onclick:{let state=state.clone();let id=job.id.clone();move |_|{let token=state.cancellations.peek().get(&id).cloned();if let Some(token)=token{token.cancel();}else{state.notify("Este trabajo ya no está activo");}}},"Cancelar"}}}}
      button{class:"settings-row",onclick:move |_|{match write_diagnostics(&logs.services.profile,&diagnostic_snapshot(&logs)){Ok(path)=>crate::native::open_folder(&logs,path.parent().unwrap_or(&logs.services.profile)),Err(error)=>logs.notify(error)}},span{class:"settings-row__main",Icon{name:"folder",size:15}span{class:"settings-row__label",span{class:"settings-row__title","Abrir logs"}span{class:"settings-row__sub","Incluye el diagnóstico actual sin cookies ni enlaces privados"}}}Icon{name:"chevron-right",size:13}}
      button{class:"settings-row",onclick:move |_|{jobs.set(refresh.services.core.jobs().unwrap_or_default());refresh.notify("Diagnóstico actualizado");},span{class:"settings-row__title","Actualizar diagnóstico"}Icon{name:"chevron-right",size:13}}
    }}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{use_app_state, Services};
    use dioxus::dioxus_core::{AttributeValue, Mutation};
    use std::cell::RefCell;
    type Capture = Rc<RefCell<Option<AppState>>>;
    fn harness() -> Element {
        let services = use_context::<Rc<Services>>();
        let capture = use_context::<Capture>();
        let state = use_app_state(services);
        *capture.borrow_mut() = Some(state.clone());
        use_context_provider(|| state);
        rsx! {SettingsPage{}}
    }
    #[tokio::test]
    async fn settings_restore_legacy_controls_and_validate_number_edits_without_borrow_panics() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let mut settings = services.core.settings().unwrap();
        settings.show_advanced_tools = true;
        settings.developer_mode = true;
        services.core.save_settings(&settings).unwrap();
        let capture: Capture = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(harness);
        dom.insert_any_root_context(Box::new(services.clone()));
        dom.insert_any_root_context(Box::new(capture.clone()));
        let mutations = dom.rebuild_to_vec();
        let html = dioxus_ssr::render(&dom);
        for label in [
            "Calidad de audio",
            "Calidad de vídeo",
            "Máxima disponible",
            "2160p",
            "Biblioteca",
            "Cookies del navegador",
            "firefox",
            "whale",
            "Reintentos de descarga",
            "Espera de red (segundos)",
            "Procesos pesados",
            "Plantilla de archivo",
            "Diagnóstico",
            "Abrir logs",
            "Crear plugin",
        ] {
            assert!(html.contains(label), "missing {label}");
        }
        assert_eq!(
            services.core.settings().unwrap().max_concurrent_downloads,
            3
        );
        let input = mutations
            .edits
            .iter()
            .find_map(|edit| match edit {
                Mutation::SetAttribute {
                    name: "aria-label",
                    value: AttributeValue::Text(label),
                    id,
                    ..
                } if label == "Reintentos de descarga" => Some(*id),
                _ => None,
            })
            .unwrap();
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let event: Event<dyn std::any::Any> = Event::new(
            Rc::new(dioxus::html::PlatformEventData::new(Box::new(
                dioxus::html::SerializedFormData {
                    value: "999".into(),
                    values: Vec::new(),
                    valid: false,
                },
            ))),
            true,
        );
        dom.runtime().handle_event("input", event, input);
        dom.render_immediate_to_vec();
        let blur: Event<dyn std::any::Any> = Event::new(
            Rc::new(dioxus::html::PlatformEventData::new(Box::new(
                dioxus::html::SerializedFocusData::default(),
            ))),
            true,
        );
        dom.runtime().handle_event("blur", blur, input);
        dom.render_immediate_to_vec();
        assert_eq!(services.core.settings().unwrap().download_retries, 2);
        assert!(capture
            .borrow()
            .as_ref()
            .unwrap()
            .notice
            .peek()
            .as_ref()
            .unwrap()
            .contains("entre 0 y 10"));
    }
    #[tokio::test]
    async fn diagnostic_report_never_serializes_connection_credentials_or_private_urls() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let mut settings = services.core.settings().unwrap();
        settings.extra.insert(
            "youtube_proxy".into(),
            serde_json::json!("https://user:secret-value@private.example"),
        );
        settings.extra.insert(
            "youtube_cookie_file".into(),
            serde_json::json!("/private/secret-file.txt"),
        );
        services.core.save_settings(&settings).unwrap();
        let capture: Capture = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(harness);
        dom.insert_any_root_context(Box::new(services));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let state = capture.borrow().as_ref().unwrap().clone();
        let report = diagnostic_snapshot(&state);
        let path = write_diagnostics(profile.path(), &report).unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        for secret in ["secret-value", "secret-file", "private.example"] {
            assert!(!text.contains(secret));
        }
        assert_eq!(report["connection"]["cookie_file_configured"], true);
    }
}
