use crate::{
    app::{artwork_src, EmptyState, Thumb},
    icons::Icon,
    processing::{separator_label, start_processing, ProcessingTask, REQUIRES_METAL},
    state::{size_label, AppState, Page},
};
use dioxus::prelude::*;
use std::{
    collections::HashSet,
    io::{Read, Write},
    path::Path,
};
use yt_core::{Job, JobStatus, Track};

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Flac,
    Mp3,
    Wav,
}
impl OutputFormat {
    fn label(self) -> &'static str {
        match self {
            Self::Flac => "FLAC",
            Self::Mp3 => "MP3",
            Self::Wav => "WAV",
        }
    }
    fn audio(self) -> yt_audio::AudioFormat {
        match self {
            Self::Flac => yt_audio::AudioFormat::Flac,
            Self::Wav => yt_audio::AudioFormat::Wav,
            Self::Mp3 => yt_audio::AudioFormat::Mp3(320),
        }
    }
}

const PRESETS: [(&str, f64, f64, f64); 4] = [
    ("Clásica", 7.0, 1.0, 3.0),
    ("Suave", 4.0, 1.0, 2.0),
    ("Súper aguda", 12.0, 1.0, 5.0),
    ("Acelerada", 9.0, 1.25, 4.0),
];
static CHIPMUNK_VALUES: GlobalSignal<(f64, f64, f64)> = Signal::global(|| (7.0, 1.0, 3.0));

#[component]
pub fn ProcessingPage() -> Element {
    let state = use_context::<AppState>();
    let mut selected = use_signal(String::new);
    let mut query = use_signal(String::new);
    let mut format = use_signal(|| OutputFormat::Flac);
    let mut jobs = use_signal(|| state.services.core.jobs().unwrap_or_default());
    let mut job_error = use_signal(|| None::<String>);
    let model = use_hook(model_information);
    let polling = state.clone();
    use_hook(move || {
        spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                if *polling.page.peek() != Page::Stems {
                    continue;
                }
                match polling.services.core.jobs() {
                    Ok(current) => {
                        if *jobs.peek() != current {
                            jobs.set(current);
                        }
                        if job_error.peek().is_some() {
                            job_error.set(None);
                        }
                    }
                    Err(error) => {
                        let error = error.to_string();
                        if job_error.peek().as_ref() != Some(&error) {
                            job_error.set(Some(error));
                        }
                    }
                }
            }
        })
    });
    let library = state.library.read().clone();
    let all_jobs = jobs.read().clone();
    let selected_id = selected.read().clone();
    let mut excluded = all_jobs
        .iter()
        .filter(|job| matches!(job.kind.as_str(), "stems" | "stem_separation"))
        .flat_map(output_ids)
        .map(|(_, id)| id)
        .collect::<HashSet<_>>();
    for track in &library {
        if state
            .services
            .core
            .track_metadata(&track.id)
            .ok()
            .is_some_and(|value| value["playlist"] == "Separaciones")
        {
            excluded.insert(track.id.clone());
        }
    }
    let sources = source_tracks(&library, &query.read(), &excluded);
    let selection = library
        .iter()
        .find(|track| track.id == selected_id && !track.missing && !excluded.contains(&track.id))
        .cloned();
    let visible_sources = sources.iter().take(60).cloned().collect::<Vec<_>>();
    let visible_jobs = display_jobs(&all_jobs);
    let current_format = *format.read();
    let disabled = selection.is_none() || model.is_err();
    let title = selection
        .as_ref()
        .map(|track| track.title.as_str())
        .unwrap_or("Elige una de la lista");
    let selection_is_empty = selection.is_none();
    let selection_for_start = selection.clone();
    let start_state = state.clone();
    let model_label = separator_label();
    let device = if cfg!(target_os = "windows") {
        "tu PC"
    } else {
        "tu Mac"
    };
    rsx! {
        style {{include_str!("processing_ui.css")}}
        div {class:"page-shell processing-page",
            div {class:"toolbar",div {class:"toolbar__left",h1 {class:"page-title","Stems"}span {class:"page-count","{model_label}" if REQUIRES_METAL {" · Metal"}}}}
            div {class:"page-subtitle processing-subtitle","Separa la voz y el instrumental de cualquier canción, directamente en {device}."}
            div {class:"stems-layout",
                div {class:"stems-main",
                    div {class:"input-search-wrap stems-search",
                        span {class:"input-search-icon",Icon{name:"search",size:14}}
                        input {class:"input-search",placeholder:"Buscar en tu biblioteca",aria_label:"Buscar en tu biblioteca",value:"{query}",oninput:move|event|query.set(event.value())}
                        if !query.read().is_empty(){button {class:"input-search-clear",aria_label:"Limpiar",onclick:move |_|query.set(String::new()),"×"}}
                    }
                    if visible_sources.is_empty(){EmptyState {title:"No hay canciones para separar",hint:"Descarga audio o vídeo desde Buscar y aparecerá aquí.",compact:true}}
                    for track in visible_sources {
                        button {key:"{track.id}",class:if selected_id==track.id{"row row--active processing-source"}else{"row processing-source"},aria_pressed:format!("{}",selected_id==track.id),
                            onclick:{let id=track.id.clone();move |_|{let next=if *selected.peek()==id{String::new()}else{id.clone()};selected.set(next);}},
                            Thumb {src:artwork_src(&track),size:"sm",icon_size:Some(14)}
                            div {class:"row__meta",div {class:if selected_id==track.id{"row__title row__title--active"}else{"row__title"},"{track.title}"}div {class:"row__sub",if track.artist.is_empty(){"{track.filename}"}else{"{track.artist}"}}}
                            span {class:"row__detail","{size_label(track.size_bytes)}"}
                        }
                    }
                }
                div {class:"stems-side",
                    div {class:"settings-section processing-controls",
                        div {class:"settings-section__header",div {class:"settings-section__icon",Icon{name:"scissors",size:15}}
                            div {div {class:"settings-section__title","Extracción local"}div {class:"settings-section__sub",match &model {Ok(bytes)=>format!("Modelo listo · {}",model_size_label(*bytes)),Err(_)=>"Modelo no disponible en esta edición".into()}}}
                        }
                        div {class:"stems-control",
                            div {class:"stems-selected",span {class:"stems-selected__label","Canción seleccionada"}span {class:if selection_is_empty{"stems-selected__title processing-placeholder"}else{"stems-selected__title"},"{title}"}}
                            div {class:"segmented stems-format",role:"group",aria_label:"Formato de las separaciones",
                                for option in [OutputFormat::Flac,OutputFormat::Mp3,OutputFormat::Wav] {button {class:if current_format==option{"segment active"}else{"segment"},aria_pressed:format!("{}",current_format==option),onclick:move |_|format.set(option),"{option.label()}"}}
                            }
                            button {class:"btn-accent stems-start",disabled,onclick:move |_|{
                                if let Some(track)=&selection_for_start {
                                    if start_processing(start_state.clone(),track.id.clone(),ProcessingTask::StemsFormat(current_format.audio())) {
                                        if let Ok(current)=start_state.services.core.jobs(){jobs.set(current);}
                                    }
                                }
                            },Icon{name:"scissors",size:13}"Separar"}
                        }
                    }
                    if let Some(error)=job_error.read().as_ref(){p {class:"task-row__error",role:"alert","No se pudieron actualizar los trabajos: {error}"}}
                    if !visible_jobs.is_empty(){div {div {class:"section-header",h2 {class:"section-title processing-jobs-title","Trabajos"}}for job in visible_jobs {ProcessingJobCard {key:"{job.id}",job,library:library.clone()}}}}
                }
            }
        }
    }
}
fn model_size_label(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else {
        format!("{:.0} MB", bytes as f64 / 1_048_576.0)
    }
}

#[component]
pub fn ChipmunkDialog(track: Track, onclose: EventHandler<()>) -> Element {
    let state = use_context::<AppState>();
    let initial = use_hook(|| *CHIPMUNK_VALUES.read());
    let mut pitch = use_signal(|| initial.0);
    let mut speed = use_signal(|| initial.1);
    let mut brightness = use_signal(|| initial.2);
    use_effect(move || {
        let values = (*pitch.read(), *speed.read(), *brightness.read());
        *CHIPMUNK_VALUES.write() = values;
    });
    let values = (*pitch.read(), *speed.read(), *brightness.read());
    let title = track.title.clone();
    let source_id = track.id.clone();
    let submit = move |_| {
        let parameters =
            yt_audio::VariantOptions::chipmunk(*pitch.peek(), *speed.peek(), *brightness.peek());
        if start_processing(
            state.clone(),
            source_id.clone(),
            ProcessingTask::Variant(parameters, "Ardilla"),
        ) {
            onclose.call(());
        }
    };
    rsx! {
        style {{include_str!("processing_ui.css")}}
        div {class:"modal-backdrop",onclick:move |_|onclose.call(()),
            div {class:"modal-panel chipmunk-dialog",role:"dialog",aria_modal:"true",aria_label:"Versión Alvin y las ardillas",tabindex:"-1",
                onclick:|event|event.stop_propagation(),onkeydown:move |event|{if event.key()==Key::Escape{event.stop_propagation();onclose.call(());}},
                div {class:"modal-header",
                    div {class:"processing-dialog-heading",Thumb {src:artwork_src(&track),size:"lg",icon_size:Some(20)}div {class:"processing-dialog-meta",div {class:"modal-title","Versión ardilla"}div {class:"chipmunk-dialog__song","{title}"}}}
                    button {class:"btn-icon-sm",title:"Cerrar",aria_label:"Cerrar versión ardilla",onmounted:|event|async move{let _=event.set_focus(true).await;},onclick:move |_|onclose.call(()),Icon{name:"close",size:14}}
                }
                div {class:"chipmunk-presets",role:"group",aria_label:"Estilos",
                    for (label,p,v,b) in PRESETS {button {r#type:"button",class:if values==(p,v,b){"chipmunk-preset chipmunk-preset--active"}else{"chipmunk-preset"},aria_pressed:format!("{}",values==(p,v,b)),onclick:move |_|{pitch.set(p);speed.set(v);brightness.set(b);},"{label}"}}
                }
                ChipmunkSlider {label:"Tono",value:pitch,display:format!("+{} semitonos",number_label(values.0)),minimum:"1",maximum:"12",step:"0.5"}
                ChipmunkSlider {label:"Velocidad",value:speed,display:format!("{:.2}×",values.1),minimum:"0.5",maximum:"2",step:"0.05"}
                ChipmunkSlider {label:"Brillo",value:brightness,display:if values.2==0.0{"Sin cambio".into()}else{format!("+{} dB",number_label(values.2))},minimum:"0",maximum:"12",step:"1"}
                p {class:"chipmunk-dialog__hint","Se guarda como una canción nueva en la carpeta «Ardillas»; el original no cambia."}
                div {class:"modal-actions",button {r#type:"button",class:"btn-secondary",onclick:move |_|onclose.call(()),"Cancelar"}button {r#type:"button",class:"btn-accent",disabled:track.missing,onclick:submit,"Crear versión"}}
            }
        }
    }
}

#[component]
fn ChipmunkSlider(
    label: &'static str,
    mut value: Signal<f64>,
    display: String,
    minimum: &'static str,
    maximum: &'static str,
    step: &'static str,
) -> Element {
    rsx! {label {class:"chipmunk-slider",span {class:"chipmunk-slider__head",span {"{label}"}span {class:"chipmunk-slider__value","{display}"}}
        input {r#type:"range",min:minimum,max:maximum,step,value:"{value}",aria_label:label,aria_valuetext:display,oninput:move |event|{if let Ok(next)=event.value().parse::<f64>() {if next.is_finite(){value.set(next.clamp(minimum.parse().unwrap_or_default(),maximum.parse().unwrap_or(12.0)));}}}}
    }}
}

#[component]
fn ProcessingJobCard(job: Job, library: Vec<Track>) -> Element {
    let state = use_context::<AppState>();
    let mut canceling = use_signal(|| false);
    let active = !job.status.is_terminal();
    let requested = *canceling.read() && active;
    let (status, tone) = job_status(&job, requested);
    let progress = if job.progress.is_finite() {
        job.progress.clamp(0.0, 100.0)
    } else {
        0.0
    };
    let title = job.payload["title"]
        .as_str()
        .unwrap_or("Procesamiento de audio");
    let detail = if !job.message.is_empty() {
        job.message.clone()
    } else {
        job.payload["output_format"]
            .as_str()
            .or_else(|| job.payload["format"].as_str())
            .unwrap_or_default()
            .into()
    };
    let outputs = output_ids(&job)
        .into_iter()
        .filter_map(|(label, id)| {
            library
                .iter()
                .find(|track| track.id == id)
                .cloned()
                .map(|track| (label, track))
        })
        .collect::<Vec<_>>();
    let id = job.id.clone();
    rsx! {div {class:"task-row processing-job",
        div {class:"task-row__body",div {class:"task-row__title","{title}"}
            div {class:"task-row__status",span {class:if active{"processing-status-icon spinner"}else{"processing-status-icon"},style:"color:{tone}",StatusIcon {status:job.status}}span {style:"color:{tone}","{status}"}if !detail.trim().is_empty() && detail.trim() != status {span {"· {detail}"}}}
            if active {div {class:"progress-track",role:"progressbar",aria_label:format!("Progreso de {title}"),aria_valuemin:"0",aria_valuemax:"100",aria_valuenow:format!("{progress}"),div {class:"progress-fill",style:"width:{progress}%"}}}
            if !job.error.is_empty()&&matches!(job.status,JobStatus::Error|JobStatus::Interrupted){div {class:"task-row__error","{job.error}"}}
            if !outputs.is_empty(){div {class:"stems-output-list",
                for (label,track) in outputs {button {class:"stems-output",disabled:track.missing,title:format!("Guardar {label}: {}",track.filename),onclick:{let state=state.clone();move |_|save_generated_output(state.clone(),track.clone())},Icon{name:"download",size:11}"{label}"}}
            }}
        }
        if active {span {class:"task-row__pct","{progress:.0}%"}button {class:"btn-icon-sm",title:"Cancelar",aria_label:format!("Cancelar {title}"),disabled:requested,onclick:move |_|{
            let token=state.cancellations.peek().get(&id).cloned();
            if let Some(token)=token {token.cancel();canceling.set(true);}else{state.notify_warning("El trabajo ya terminó o no está activo en esta sesión.");}
        },Icon{name:"close",size:13}}}
    }}
}

#[component]
fn StatusIcon(status: JobStatus) -> Element {
    if status == JobStatus::Completed {
        return rsx! {svg {width:"12",height:"12",view_box:"0 0 24 24",fill:"none",stroke:"currentColor",stroke_width:"2",stroke_linecap:"round","aria-hidden":"true",circle{cx:"12",cy:"12",r:"9"}path{d:"m8 12 3 3 5-6"}}};
    }
    if status == JobStatus::Canceled {
        return rsx! {Icon{name:"close",size:12}};
    }
    if matches!(status, JobStatus::Error | JobStatus::Interrupted) {
        return rsx! {svg {width:"12",height:"12",view_box:"0 0 24 24",fill:"none",stroke:"currentColor",stroke_width:"2","aria-hidden":"true",circle {cx:"12",cy:"12",r:"9"}path {d:"M12 7v6m0 3v1"}}};
    }
    rsx! {svg {width:"12",height:"12",view_box:"0 0 24 24",fill:"none",stroke:"currentColor",stroke_width:"2",stroke_linecap:"round","aria-hidden":"true",path {d:"M21 12a9 9 0 1 1-9-9"}}}
}

fn source_tracks(library: &[Track], query: &str, excluded: &HashSet<String>) -> Vec<Track> {
    let query = query.trim().to_lowercase();
    library
        .iter()
        .filter(|track| {
            !track.missing
                && !track.path.as_os_str().is_empty()
                && matches!(track.media_type.as_str(), "audio" | "video")
                && !excluded.contains(&track.id)
        })
        .filter(|track| {
            query.is_empty()
                || format!("{} {} {}", track.title, track.artist, track.filename)
                    .to_lowercase()
                    .contains(&query)
        })
        .cloned()
        .collect()
}
fn output_ids(job: &Job) -> Vec<(String, String)> {
    let mut output = [
        ("Vocals", "vocals"),
        ("Instrumental", "instrumental"),
        ("Resultado", "track"),
    ]
    .into_iter()
    .filter_map(|(label, key)| {
        job.result[key]
            .as_str()
            .map(|id| (label.to_owned(), id.to_owned()))
    })
    .collect::<Vec<_>>();
    for value in [&job.result["outputs"], &job.payload["outputs"]] {
        if let Some(items) = value.as_array() {
            for item in items {
                if let Some(id) = item["track_id"].as_str().filter(|id| !id.is_empty()) {
                    if !output.iter().any(|(_, existing)| existing == id) {
                        output.push((
                            item["label"].as_str().unwrap_or("Resultado").into(),
                            id.into(),
                        ));
                    }
                }
            }
        }
    }
    output
}
fn display_jobs(jobs: &[Job]) -> Vec<Job> {
    let processing = jobs
        .iter()
        .filter(|job| matches!(job.kind.as_str(), "stems" | "stem_separation"))
        .collect::<Vec<_>>();
    processing
        .iter()
        .filter(|job| !job.status.is_terminal())
        .copied()
        .chain(
            processing
                .iter()
                .filter(|job| job.status.is_terminal())
                .take(8)
                .copied(),
        )
        .cloned()
        .collect()
}
fn job_status(job: &Job, canceling: bool) -> (&'static str, &'static str) {
    if canceling {
        return ("Cancelando", "var(--text-tertiary)");
    }
    match job.status {
        JobStatus::Pending => ("En cola", "var(--text-tertiary)"),
        JobStatus::Running => (
            if matches!(job.kind.as_str(), "stems" | "stem_separation") {
                "Separando"
            } else {
                "Procesando"
            },
            "var(--accent)",
        ),
        JobStatus::Completed => ("Completado", "var(--system-green)"),
        JobStatus::Error => ("Error", "var(--system-red)"),
        JobStatus::Canceled => ("Cancelado", "var(--text-tertiary)"),
        JobStatus::Interrupted => ("Interrumpido", "var(--system-red)"),
    }
}
fn number_label(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}
fn model_information() -> Result<u64, String> {
    #[cfg(any(
        feature = "native-stems",
        all(feature = "metal-stems", target_os = "macos")
    ))]
    {
        let path = crate::processing::model_path()?;
        let metadata = std::fs::metadata(path).map_err(|_| {
            "No se encontró el modelo. Reinstala la edición con herramientas de audio.".to_owned()
        })?;
        if metadata.is_file() {
            Ok(metadata.len())
        } else {
            Err("La ruta del modelo no es un archivo.".into())
        }
    }
    #[cfg(not(any(
        feature = "native-stems",
        all(feature = "metal-stems", target_os = "macos")
    )))]
    {
        Err("La separación y CALIDAD requieren la edición con herramientas de audio. Las variaciones y la versión ardilla están disponibles.".into())
    }
}
fn save_generated_output(state: AppState, track: Track) {
    dioxus::core::spawn_forever(async move {
        let extension = track.extension.trim_start_matches('.').to_owned();
        let Some(output) = rfd::AsyncFileDialog::new()
            .set_title("Guardar resultado")
            .set_file_name(&track.filename)
            .add_filter("Audio", &[extension.as_str()])
            .save_file()
            .await
        else {
            return;
        };
        let destination = output.path().to_owned();
        let result =
            tokio::task::spawn_blocking(move || copy_completed_output(&track.path, &destination))
                .await;
        match result {
            Ok(Ok(())) => state.notify_success("Resultado guardado"),
            Ok(Err(error)) => state.notify_error(format!("No se pudo guardar: {error}")),
            Err(error) => state.notify_error(format!("No se pudo guardar: {error}")),
        }
    });
}
fn copy_completed_output(source: &Path, destination: &Path) -> std::io::Result<()> {
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut reader = std::fs::File::open(source)?;
    let mut output = tempfile::Builder::new()
        .prefix(".ytu-export-")
        .tempfile_in(parent)?;
    let mut buffer = [0; 128 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count])?;
    }
    output.as_file().sync_all()?;
    output
        .persist_noclobber(destination)
        .map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{use_app_state, Services};
    use std::rc::Rc;

    fn harness() -> Element {
        let services = use_context::<Rc<Services>>();
        let state = use_app_state(services);
        let track = state.library.read().first().cloned();
        use_context_provider(|| state);
        rsx! {
            ProcessingPage {}
            if let Some(track)=track {ChipmunkDialog {track,onclose:|_|{}}}
        }
    }

    #[tokio::test]
    async fn original_stems_layout_and_chipmunk_controls_render_with_real_jobs() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let track = services
            .core
            .import_bytes(
                "Canción.wav",
                include_bytes!("../../yt-quality/tests/fixtures/input.wav"),
            )
            .unwrap();
        let running = services
            .core
            .create_job(
                "stems",
                &serde_json::json!({"title":"Separación en curso","track_id":track.id}),
            )
            .unwrap();
        services
            .core
            .update_job(
                &running.id,
                JobStatus::Running,
                36.0,
                "Separando vocales",
                &serde_json::json!({}),
                "",
            )
            .unwrap();
        let done = services
            .core
            .create_job(
                "stems",
                &serde_json::json!({"title":"Separación terminada"}),
            )
            .unwrap();
        services
            .core
            .update_job(
                &done.id,
                JobStatus::Completed,
                100.0,
                "Completado",
                &serde_json::json!({"vocals":track.id}),
                "",
            )
            .unwrap();
        services
            .core
            .create_job("stems", &serde_json::json!({"title":"Versión en cola"}))
            .unwrap();
        let mut dom = VirtualDom::new(harness);
        dom.insert_any_root_context(Box::new(services));
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        for value in [
            "stems-layout",
            "stems-main",
            "stems-side",
            "Buscar en tu biblioteca",
            "Extracción local",
            "Canción seleccionada",
            "FLAC",
            "MP3",
            "WAV",
            "En cola",
            "Separando",
            "36%",
            "Guardar Vocals",
            "Versión Alvin y las ardillas",
            "Clásica",
            "Suave",
            "Súper aguda",
            "Acelerada",
            "+7 semitonos",
            "1.00×",
            "+3 dB",
            "Crear versión",
        ] {
            assert!(html.contains(value), "missing processing UI: {value}");
        }
        assert!(
            html.contains("aria-valuenow=\"36"),
            "{}",
            html.split("role=\"progressbar\"")
                .nth(1)
                .unwrap_or("missing progressbar")
                .chars()
                .take(250)
                .collect::<String>()
        );
        assert!(!html.contains("http://127.0.0.1"));
        assert_eq!(html.matches("Completado").count(), 1);
        assert_eq!(OutputFormat::Mp3.audio(), yt_audio::AudioFormat::Mp3(320));
    }

    #[test]
    fn processing_sources_filter_missing_remote_and_previous_stems_without_losing_search() {
        let profile = tempfile::tempdir().unwrap();
        let core = yt_core::AppCore::open(profile.path()).unwrap();
        let first = core.import_bytes("Árbol.wav", b"audio fixture").unwrap();
        let mut missing = first.clone();
        missing.id = "missing".into();
        missing.missing = true;
        let mut remote = first.clone();
        remote.id = "remote".into();
        remote.path.clear();
        let mut stem = first.clone();
        stem.id = "stem".into();
        let library = vec![first.clone(), missing, remote, stem];
        let visible = source_tracks(&library, " ÁRBOL ", &HashSet::from(["stem".into()]));
        assert_eq!(
            visible
                .iter()
                .map(|track| track.id.as_str())
                .collect::<Vec<_>>(),
            vec![first.id.as_str()]
        );
        assert!(source_tracks(&library, "unmatched", &HashSet::new()).is_empty());
    }

    #[test]
    fn active_work_precedes_eight_recent_results_and_result_keys_are_real_track_ids() {
        let profile = tempfile::tempdir().unwrap();
        let core = yt_core::AppCore::open(profile.path()).unwrap();
        let mut completed = Vec::new();
        for n in 0..12 {
            let job = core
                .create_job("stem_separation", &serde_json::json!({"title":n}))
                .unwrap();
            completed.push(
                core.update_job(
                    &job.id,
                    JobStatus::Completed,
                    100.0,
                    "Hecho",
                    &serde_json::json!({"track":format!("track-{n}")}),
                    "",
                )
                .unwrap(),
            );
        }
        let pending = core.create_job("stems", &serde_json::json!({})).unwrap();
        completed.push(pending.clone());
        completed.push(core.create_job("download", &serde_json::json!({})).unwrap());
        let shown = display_jobs(&completed);
        assert_eq!(shown.len(), 9);
        assert_eq!(shown[0].id, pending.id);
        assert_eq!(job_status(&shown[0], false).0, "En cola");
        assert_eq!(job_status(&shown[0], true).0, "Cancelando");
        assert_eq!(
            output_ids(&shown[1]),
            vec![("Resultado".into(), "track-0".into())]
        );
    }

    #[test]
    fn saving_processed_output_keeps_exact_bytes_and_never_replaces_existing_files() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("vocals.mp3");
        let output = directory.path().join("saved.mp3");
        let bytes = b"ID3\0artwork\0private-tags\xff\xfbencoded-audio";
        std::fs::write(&source, bytes).unwrap();
        copy_completed_output(&source, &output).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), bytes);
        std::fs::write(&source, b"new source").unwrap();
        assert!(copy_completed_output(&source, &output).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), bytes);
        assert!(copy_completed_output(&source, &source).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), b"new source");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }
}
