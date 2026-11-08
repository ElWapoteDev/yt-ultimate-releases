use crate::{icons::Icon, state::AppState};
use dioxus::prelude::*;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use yt_updater::{CancellationToken, Edition, UpdateInfo, Updater, VerifiedUpdate};

#[component]
pub fn UpdatePanel() -> Element {
    let state = use_context::<AppState>();
    let updater = use_hook(|| {
        Updater::with_edition(
            env!("CARGO_PKG_VERSION"),
            state.services.profile.join("Updates"),
            if cfg!(feature = "native-stems") {
                Edition::Full
            } else {
                Edition::Base
            },
        )
        .map_err(|e| e.to_string())
    });
    let mut available = use_signal(|| None::<UpdateInfo>);
    let mut ready = use_signal(|| None::<VerifiedUpdate>);
    let mut busy = use_signal(|| false);
    let mut status = use_signal(|| format!("Versión instalada: {}", env!("CARGO_PKG_VERSION")));
    let mut cancellation = use_signal(|| None::<CancellationToken>);
    let progress = use_hook(|| Arc::new(AtomicU64::new(0)));
    let mut percent = use_signal(|| 0u64);
    let poll_progress = progress.clone();
    use_hook(move || {
        spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                if *busy.peek() {
                    let value = poll_progress.load(Ordering::Relaxed);
                    if *percent.peek() != value {
                        percent.set(value);
                    }
                }
            }
        })
    });
    let check_updater = updater.clone();
    let download_updater = updater.clone();
    let install_updater = updater.clone();
    let update = available.read().clone();
    let downloaded = ready.read().clone();
    rsx! { section {class:"settings-section",
        div {class:"settings-section__header",div {class:"settings-section__icon",Icon{name:"download",size:19}}
            div {div{class:"settings-section__title","Actualizaciones"}div{class:"settings-section__sub","Nuevas versiones de YT Ultimate"}}
        }
        p {class:"rust-settings-note",role:"status","{status}"}
        if let Err(error)=&updater {p{class:"rust-settings-note rust-danger","{error}"}}
        button {class:"btn-secondary",disabled:*busy.read()||updater.is_err(),onclick:move |_|{
            let Ok(updater)=check_updater.clone()else{return;};busy.set(true);status.set("Buscando actualizaciones…".into());
            spawn(async move{let result=updater.check().await;busy.set(false);match result{
                Ok(Some(info))=>{status.set(format!("Versión {} disponible",info.version));available.set(Some(info));ready.set(None);},
                Ok(None)=>{status.set("Tienes la última versión disponible para esta edición".into());available.set(None);ready.set(None);},
                Err(error)=>status.set(error.to_string()),
            }});
        },"Buscar actualizaciones"}
        if let Some(info)=update {
            if downloaded.is_none() {
                button {class:"btn-accent",style:"margin-left:8px",disabled:*busy.read(),onclick:move |_|{
                    let Ok(updater)=download_updater.clone()else{return;};let info=info.clone();let progress=progress.clone();progress.store(0,Ordering::Relaxed);percent.set(0);
                    let token=CancellationToken::new();cancellation.set(Some(token.clone()));busy.set(true);status.set("Descargando actualización…".into());
                    spawn(async move{let result=updater.download(&info,token,move |p|{progress.store(p.downloaded_bytes.saturating_mul(100).checked_div(p.total_bytes).unwrap_or(0),Ordering::Relaxed);}).await;
                        busy.set(false);cancellation.set(None);match result{Ok(update)=>{status.set("Descarga verificada. Ya puedes abrir el instalador.".into());ready.set(Some(update));},Err(error)=>status.set(error.to_string())}
                    });
                },"Descargar actualización"}
            }
        }
        if let Some(update)=downloaded {
            button {class:"btn-accent",style:"margin-left:8px",disabled:*busy.read(),onclick:move |_|{
                let Ok(updater)=install_updater.clone()else{return;};let update=update.clone();busy.set(true);status.set("Comprobando instalador…".into());
                spawn(async move{let result=tokio::task::spawn_blocking(move||updater.open_installer(&update)).await;busy.set(false);status.set(match result{Ok(Ok(()))=>"Instalador abierto. Sigue los pasos de instalación.".into(),Ok(Err(error))=>error.to_string(),Err(error)=>error.to_string()});});
            },"Abrir instalador"}
        }
        if cancellation.read().is_some(){
            progress {class:"rust-progress",style:"margin-top:16px",max:"100",value:"{percent}"}
            button {class:"btn-secondary",onclick:move |_|if let Some(token)=cancellation.read().as_ref(){token.cancel();},"Cancelar descarga"}
        }
    } }
}
