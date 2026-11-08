use crate::{
    icons::Icon,
    state::{AppState, Page},
};
use dioxus::prelude::*;
use std::path::Path;
use yt_core::{AppCore, JobStatus, Track};
use yt_media::CancellationToken;

#[derive(Clone, Copy)]
pub enum ProcessingTask {
    Stems,
    Quality,
    Variant(yt_audio::VariantOptions, &'static str),
}
impl ProcessingTask {
    fn kind(self) -> &'static str {
        match self {
            Self::Stems => "stems",
            Self::Quality => "quality",
            Self::Variant(..) => "variant",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Stems => "Stems",
            Self::Quality => "CALIDAD",
            Self::Variant(_, label) => label,
        }
    }
}

#[component]
pub fn ProcessingPage() -> Element {
    let s = use_context::<AppState>();
    let mut selected = use_signal(String::new);
    let mut mode = use_signal(|| {
        if cfg!(feature = "native-stems") {
            "stems".to_owned()
        } else {
            "balanced".to_owned()
        }
    });
    let mut pitch = use_signal(|| 7.0f64);
    let mut speed = use_signal(|| 1.0f64);
    let mut brightness = use_signal(|| 3.0f64);
    let mut jobs = use_signal(|| s.services.core.jobs().unwrap_or_default());
    let polling = s.clone();
    use_hook(move || {
        spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(750)).await;
                if *polling.page.peek() == Page::Stems {
                    if let Ok(current) = polling.services.core.jobs() {
                        if *jobs.peek() != current {
                            jobs.set(current);
                        }
                    }
                }
            }
        })
    });
    let tracks = s.library.read().clone();
    let start_state = s.clone();
    let processing = jobs
        .read()
        .iter()
        .filter(|j| matches!(j.kind.as_str(), "stems" | "quality" | "variant"))
        .cloned()
        .collect::<Vec<_>>();
    let running = processing.iter().any(|j| !j.status.is_terminal());
    rsx! { div { class:"page-shell",
        div { class:"page-header", h1 { class:"page-title", "Stems" } }
        p { class:"rust-settings-note", "Procesa el audio en tu equipo. Cada resultado se guarda como una canción nueva." }
        div { class:"rust-form",
            select { class:"rust-inline-input", style:"flex:1", aria_label:"Canción para procesar", value:"{selected}", onchange:move |e|selected.set(e.value()),
                option { value:"", "Selecciona una canción" }
                for track in tracks { option { value:"{track.id}", "{track.title}" } }
            }
            select { class:"rust-inline-input", aria_label:"Procesamiento", value:"{mode}", onchange:move |e|mode.set(e.value()),
                if cfg!(feature="native-stems") { option { value:"stems", "Vocales e instrumental" } option { value:"quality", "CALIDAD" } }
                option { value:"balanced", "Variación equilibrada" } option { value:"strong", "Variación intensa" } option { value:"chipmunk", "Versión ardilla" }
            }
            button { class:"btn-accent", disabled:selected.read().is_empty()||running, onclick:move |_| {
                let id=selected.read().clone(); let selection=mode.read().clone();
                let task=match selection.as_str() {
                    "stems"=>ProcessingTask::Stems, "quality"=>ProcessingTask::Quality,
                    "chipmunk"=>ProcessingTask::Variant(yt_audio::VariantOptions::chipmunk(*pitch.read(),*speed.read(),*brightness.read()),"Ardilla"),
                    value=>ProcessingTask::Variant(yt_audio::VariantOptions::variation(&format!("{id}-{}",chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()),value=="strong"),"Variación"),
                }; start_processing(start_state.clone(),id,task);
            }, Icon { name:"scissors", size:14 } "Procesar" }
        }
        if *mode.read()=="chipmunk" {
            div { class:"chipmunk-presets",
                for (label,p,v,b) in [("Clásica",7.0,1.0,3.0),("Suave",4.0,1.0,2.0),("Súper aguda",12.0,1.0,5.0),("Acelerada",9.0,1.25,4.0)] {
                    button { class:"btn-secondary", onclick:move |_|{pitch.set(p);speed.set(v);brightness.set(b);}, "{label}" }
                }
            }
            label { class:"chipmunk-slider", span { "Tono · +{pitch} semitonos" } input { r#type:"range",min:"1",max:"12",step:"0.5",value:"{pitch}",oninput:move |e|if let Ok(v)=e.value().parse(){pitch.set(v);} } }
            label { class:"chipmunk-slider", span { "Velocidad · {speed}×" } input { r#type:"range",min:"0.5",max:"2",step:"0.05",value:"{speed}",oninput:move |e|if let Ok(v)=e.value().parse(){speed.set(v);} } }
            label { class:"chipmunk-slider", span { "Brillo · {brightness} dB" } input { r#type:"range",min:"0",max:"12",step:"0.5",value:"{brightness}",oninput:move |e|if let Ok(v)=e.value().parse(){brightness.set(v);} } }
        }
        if !cfg!(feature="native-stems") { p { class:"rust-settings-note", "La separación vocal y CALIDAD requieren la edición con herramientas de audio." } }
        for job in processing {
            div { class:"rust-download",
                div { class:"rust-download-head", span { {job.payload["title"].as_str().unwrap_or("Procesamiento de audio")} }
                    span { class:"rust-status", match job.status { JobStatus::Completed=>"Completado",JobStatus::Error=>"Error",JobStatus::Canceled=>"Cancelado",JobStatus::Interrupted=>"Interrumpido",_=>"Procesando" } }
                    if !job.status.is_terminal() { button { class:"btn-icon-sm",title:"Cancelar procesamiento",onclick:{let state=s.clone();let id=job.id.clone();move |_|if let Some(token)=state.cancellations.read().get(&id){token.cancel();}},Icon{name:"close"} } }
                }
                progress { class:"rust-progress",max:"100",value:"{job.progress}" }
                p { class:"rust-download-detail", if job.error.is_empty(){"{job.message}"}else{"{job.error}"} }
                if job.status==JobStatus::Completed && job.kind!="stems" { p { class:"rust-settings-note", "Se verificaron las muestras de audio y la huella local. No se consultó ningún servicio de reconocimiento." } }
            }
        }
    } }
}

pub fn start_processing(mut state: AppState, id: String, task: ProcessingTask) {
    let track = match state.services.core.track(&id) {
        Ok(t) => t,
        Err(e) => {
            state.notify(e.to_string());
            return;
        }
    };
    if state
        .services
        .core
        .jobs()
        .unwrap_or_default()
        .iter()
        .any(|j| {
            matches!(j.kind.as_str(), "stems" | "quality" | "variant") && !j.status.is_terminal()
        })
    {
        state.notify("Espera a que termine el procesamiento actual");
        return;
    }
    let job=match state.services.core.create_job(task.kind(),&serde_json::json!({"title":format!("{} · {}",track.title,task.label()),"track_id":track.id})) { Ok(j)=>j,Err(e)=>{state.notify(e.to_string());return;} };
    let token = CancellationToken::new();
    state
        .cancellations
        .write()
        .insert(job.id.clone(), token.clone());
    dioxus::core::spawn_forever(async move {
        let core = state.services.core.clone();
        let library = state.services.library_dir.clone();
        let work_id = job.id.clone();
        let work_token = token.clone();
        let result = tokio::task::spawn_blocking(move || {
            let update = |percent, message: &str| {
                let _ = core.update_job(
                    &work_id,
                    JobStatus::Running,
                    percent,
                    message,
                    &serde_json::json!({}),
                    "",
                );
            };
            update(0.0, "Preparando audio");
            process(&core, &library, &track, &work_id, task, &work_token, update)
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|v| v);
        state.cancellations.write().remove(&job.id);
        let (status, output, error) = match result {
            Ok(output) => (JobStatus::Completed, output, String::new()),
            Err(error) => (
                if token.is_cancelled() {
                    JobStatus::Canceled
                } else {
                    JobStatus::Error
                },
                serde_json::json!({}),
                error,
            ),
        };
        let message = if status == JobStatus::Completed {
            "Procesamiento completado"
        } else {
            "Procesamiento detenido"
        };
        if let Err(e) = state.services.core.update_job(
            &job.id,
            status,
            if status == JobStatus::Completed {
                100.0
            } else {
                0.0
            },
            message,
            &output,
            &error,
        ) {
            state.notify(e.to_string());
        } else {
            state.notify(if error.is_empty() {
                message.to_owned()
            } else {
                error
            });
        }
        state.refresh();
    });
}

fn process(
    core: &AppCore,
    library: &Path,
    track: &Track,
    id: &str,
    task: ProcessingTask,
    token: &CancellationToken,
    progress: impl Fn(f64, &str),
) -> Result<serde_json::Value, String> {
    let work = tempfile::tempdir().map_err(|e| e.to_string())?;
    match task {
        ProcessingTask::Variant(options, label) => {
            let output = work.path().join("result.flac");
            let report = yt_audio::create_variant(&track.path, &output, options, token, |p| {
                progress(p * 99.0, "Creando variación de audio")
            })
            .map_err(|e| e.to_string())?;
            let result = publish(core, library, track, id, label, &output, "flac", token)?;
            Ok(serde_json::json!({"track":result,"report":report}))
        }
        ProcessingTask::Stems | ProcessingTask::Quality => {
            #[cfg(not(feature = "native-stems"))]
            {
                Err("Esta edición no incluye el modelo de separación de voz".into())
            }
            #[cfg(feature = "native-stems")]
            {
                let model = model_path()?;
                let prepared = work.path().join("prepared.wav");
                yt_quality::prepare_source(&track.path, &prepared, token)
                    .map_err(|e| e.to_string())?;
                progress(2.0, "Cargando modelo");
                let separator = yt_media::StemSeparator::load(model).map_err(|e| e.to_string())?;
                let vocals = work.path().join("vocals.wav");
                let instrumental = work.path().join("instrumental.wav");
                let scale = if matches!(task, ProcessingTask::Quality) {
                    65.0
                } else {
                    95.0
                };
                separator
                    .separate(&prepared, &vocals, &instrumental, token.clone(), |p| {
                        progress(
                            2.0 + p.fraction().unwrap_or(0.0) * scale,
                            "Separando vocales e instrumental",
                        )
                    })
                    .map_err(|e| e.to_string())?;
                if matches!(task, ProcessingTask::Stems) {
                    if token.is_cancelled() {
                        return Err("Procesamiento cancelado".into());
                    }
                    // Once publication starts, finish both stems together. A
                    // late cancellation must not leave a hidden partial job.
                    progress(98.0, "Guardando vocales e instrumental");
                    let commit = CancellationToken::new();
                    let vocals =
                        publish(core, library, track, id, "Vocales", &vocals, "wav", &commit)?;
                    let instrumental = match publish(
                        core,
                        library,
                        track,
                        id,
                        "Instrumental",
                        &instrumental,
                        "wav",
                        &commit,
                    ) {
                        Ok(instrumental) => instrumental,
                        Err(error) => {
                            rollback_generated(core, &vocals)?;
                            return Err(error);
                        }
                    };
                    return Ok(serde_json::json!({"vocals":vocals,"instrumental":instrumental}));
                }
                let reference = work.path().join("reference.wav");
                let output = work.path().join("result.flac");
                let dsp =
                    yt_quality::process_stems(&vocals, &instrumental, &reference, id, token, |p| {
                        progress(
                            67.0 + p.fraction * 20.0,
                            match p.stage {
                                "instrumental_eq" => "Ecualizando instrumental",
                                "mixing" => "Mezclando vocales",
                                _ => "Ajustando el audio",
                            },
                        )
                    })
                    .map_err(|e| e.to_string())?;
                yt_audio::convert(
                    &reference,
                    &output,
                    yt_audio::AudioFormat::Flac,
                    token,
                    |p| progress(87.0 + p * 7.0, "Guardando FLAC de 24 bits"),
                )
                .map_err(|e| e.to_string())?;
                progress(95.0, "Verificando audio");
                let verification = yt_quality::verify_final_flac(&reference, &output, token)
                    .map_err(|e| e.to_string())?;
                let comparison = yt_audio::compare_audio(&track.path, &output, token)
                    .map_err(|e| e.to_string())?;
                if !comparison.pcm_changed {
                    return Err("El procesamiento no cambió el audio".into());
                }
                let output = publish(core, library, track, id, "CALIDAD", &output, "flac", token)?;
                Ok(
                    serde_json::json!({"track":output,"dsp":dsp,"verification":verification,"comparison":comparison}),
                )
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn publish(
    core: &AppCore,
    library: &Path,
    track: &Track,
    id: &str,
    label: &str,
    source: &Path,
    extension: &str,
    token: &CancellationToken,
) -> Result<String, String> {
    if token.is_cancelled() {
        return Err("Procesamiento cancelado".into());
    }
    let title = format!("{} · {label}", track.title);
    let destination = library.join(format!(
        "{id}-{label}-{}",
        yt_media::safe_file_name(&title, extension)
    ));
    let mut file = tempfile::NamedTempFile::new_in(library).map_err(|e| e.to_string())?;
    std::io::copy(
        &mut std::fs::File::open(source).map_err(|e| e.to_string())?,
        file.as_file_mut(),
    )
    .map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    if token.is_cancelled() {
        return Err("Procesamiento cancelado".into());
    }
    file.persist_noclobber(&destination)
        .map_err(|e| e.to_string())?;
    let imported = match core.import_file(&destination, yt_core::ImportMode::Reference) {
        Ok(imported) => imported,
        Err(error) => {
            std::fs::remove_file(&destination)
                .map_err(|e| format!("{error}; no se pudo retirar el resultado incompleto: {e}"))?;
            return Err(error.to_string());
        }
    };
    if let Err(error) =
        core.update_track_metadata(&imported.id, &title, &track.artist, &track.artwork_url, "")
    {
        rollback_generated(core, &imported.id)?;
        return Err(error.to_string());
    }
    Ok(imported.id)
}

fn rollback_generated(core: &AppCore, id: &str) -> Result<(), String> {
    let path = core.track(id).map_err(|e| e.to_string())?.path;
    // Only called for a new file published by this operation with no-clobber.
    core.remove_track(id).map_err(|e| e.to_string())?;
    std::fs::remove_file(path)
        .map_err(|e| format!("No se pudo retirar el resultado incompleto: {e}"))
}

#[cfg(feature = "native-stems")]
fn model_path() -> Result<std::path::PathBuf, String> {
    if let Some(path) = std::env::var_os("YTU_STEM_MODEL") {
        return Ok(path.into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("Carpeta de aplicación no disponible")?;
    let mut paths = vec![
        dir.join("../Resources/models/bs-roformer-v1.pt"),
        dir.join("resources/models/bs-roformer-v1.pt"),
        dir.join("models/bs-roformer-v1.pt"),
    ];
    if cfg!(debug_assertions) {
        paths.push(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../yt-media/models/bs-roformer-v1.pt"),
        );
    }
    paths.into_iter().find(|p| p.is_file()).ok_or_else(|| {
        "No se encontró el modelo. Reinstala la edición con herramientas de audio.".into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_stems_with_long_unicode_titles_have_distinct_names() {
        let temp = tempfile::tempdir().unwrap();
        let core = AppCore::open(temp.path()).unwrap();
        let mut track = core.import_bytes("source.wav", &[1, 2, 3, 4]).unwrap();
        track.title = "界".repeat(100);
        let token = CancellationToken::new();
        let vocals = publish(
            &core,
            core.library_dir(),
            &track,
            "job",
            "Vocales",
            &track.path,
            "wav",
            &token,
        )
        .unwrap();
        let music = publish(
            &core,
            core.library_dir(),
            &track,
            "job",
            "Instrumental",
            &track.path,
            "wav",
            &token,
        )
        .unwrap();
        assert_ne!(
            core.track(&vocals).unwrap().path,
            core.track(&music).unwrap().path
        );
        let original = std::fs::read(&track.path).unwrap();
        rollback_generated(&core, &vocals).unwrap();
        rollback_generated(&core, &music).unwrap();
        assert_eq!(core.library("").unwrap().len(), 1);
        assert_eq!(std::fs::read(&track.path).unwrap(), original);
    }
}
