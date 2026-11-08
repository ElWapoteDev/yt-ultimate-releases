use crate::state::AppState;
use dioxus::prelude::*;
use std::io::{Read, Write};
use std::path::Path;
use yt_core::{AppCore, JobStatus, Track};
use yt_media::CancellationToken;

#[derive(Clone, Copy)]
pub enum ProcessingTask {
    StemsFormat(yt_audio::AudioFormat),
    Quality,
    Variant(yt_audio::VariantOptions, &'static str),
}
impl ProcessingTask {
    fn kind(self) -> &'static str {
        match self {
            Self::StemsFormat(_) => "stems",
            Self::Quality => "quality",
            Self::Variant(_, "Ardilla") => "chipmunk",
            Self::Variant(..) => "variant",
        }
    }
}

#[component]
pub fn ProcessingPage() -> Element {
    rsx! { crate::processing_ui::ProcessingPage {} }
}

pub fn start_processing(mut state: AppState, id: String, task: ProcessingTask) -> bool {
    let track = match state.services.core.track(&id) {
        Ok(t) => t,
        Err(e) => {
            state.notify(e.to_string());
            return false;
        }
    };
    let metadata = state.services.core.track_metadata(&id).unwrap_or_default();
    if track.missing || track.path.as_os_str().is_empty() {
        state.notify("El archivo ya no está disponible");
        return false;
    }
    let fingerprint = matches!(task, ProcessingTask::Quality)
        || matches!(task, ProcessingTask::Variant(_, label) if label != "Ardilla");
    if fingerprint && (track.media_type != "audio" || metadata["playlist"] == "Protegidos") {
        state.notify("Elige un audio original para modificar el fingerprint");
        return false;
    }
    if matches!(task, ProcessingTask::StemsFormat(_)) && metadata["playlist"] == "Separaciones" {
        state.notify("Esta pista ya es una separación");
        return false;
    }
    if fingerprint
        && state
            .services
            .core
            .jobs()
            .unwrap_or_default()
            .iter()
            .any(|job| {
                !job.status.is_terminal()
                    && matches!(job.kind.as_str(), "variant" | "quality" | "fingerprint")
                    && job.payload["track_id"] == id
            })
    {
        state.notify("Esta canción ya tiene una modificación en curso");
        return false;
    }
    let format = match task {
        ProcessingTask::StemsFormat(format) => format,
        ProcessingTask::Variant(_, "Ardilla") => chipmunk_format(&track),
        _ => yt_audio::AudioFormat::Flac,
    };
    let job=match state.services.core.create_job(task.kind(),&serde_json::json!({"title":track.title,"track_id":track.id,"format":format.extension().to_uppercase()})) { Ok(j)=>j,Err(e)=>{state.notify(e.to_string());return false;} };
    let task = match task {
        ProcessingTask::Variant(options, label) if label != "Ardilla" => ProcessingTask::Variant(
            yt_audio::VariantOptions::variation(&job.id, options.pitch_semitones >= 1.75),
            label,
        ),
        _ => task,
    };
    let event_prefix = match task {
        ProcessingTask::Variant(_, "Ardilla") => "processing.chipmunk",
        ProcessingTask::Variant(..) | ProcessingTask::Quality => "processing.fingerprint",
        _ => "processing.stems",
    };
    let start_message = match task {
        ProcessingTask::Quality => "CALIDAD: separando voz e instrumental…".to_owned(),
        ProcessingTask::Variant(_, "Ardilla") => {
            format!("Creando versión ardilla de \"{}\"…", track.title)
        }
        ProcessingTask::Variant(..) => "Modificando audio y verificando huella…".into(),
        _ => "Separación iniciada".into(),
    };
    state.notify(start_message);
    state.plugin_event(
        &format!("{event_prefix}.queued"),
        serde_json::json!({"job_id":job.id,"track_id":track.id,"kind":task.kind()}),
    );
    let token = CancellationToken::new();
    state
        .cancellations
        .write()
        .insert(job.id.clone(), token.clone());
    dioxus::core::spawn_forever(async move {
        let _permit = match state.services.processing_slots.acquire(&token).await {
            Ok(permit) => permit,
            Err(error) => {
                let status = if token.is_cancelled() {
                    JobStatus::Canceled
                } else {
                    JobStatus::Error
                };
                let _ = state.services.core.update_job(
                    &job.id,
                    status,
                    0.0,
                    "Procesamiento detenido",
                    &serde_json::json!({}),
                    &error,
                );
                state.cancellations.write().remove(&job.id);
                state.refresh();
                state.plugin_event(
                    &format!("{event_prefix}.stopped"),
                    serde_json::json!({"job_id":job.id,"status":status.as_str()}),
                );
                return;
            }
        };
        let core = state.services.core.clone();
        let library = state.services.library_dir.clone();
        let work_id = job.id.clone();
        let work_token = token.clone();
        let plugin_host = state
            .settings
            .peek()
            .developer_mode
            .then(|| state.services.plugins.clone().ok())
            .flatten();
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<(f64, String)>(8);
        let notification_title = track.title.clone();
        let mut worker = tokio::task::spawn_blocking(move || {
            let last_progress = std::cell::Cell::new(-1i64);
            let update = |percent, message: &str| {
                let _ = core.update_job(
                    &work_id,
                    JobStatus::Running,
                    percent,
                    message,
                    &serde_json::json!({}),
                    "",
                );
                if let Some(host) = &plugin_host {
                    let _ = host.dispatch_event(yt_plugins::PluginEvent { name: format!("{event_prefix}.progress"), payload: serde_json::json!({"job_id":work_id,"progress":percent,"message":message}) });
                }
                if fingerprint || matches!(task, ProcessingTask::Variant(_, "Ardilla")) {
                    let rounded = percent.round() as i64;
                    if rounded != last_progress.replace(rounded) {
                        let _ = progress_tx.try_send((percent, message.to_owned()));
                    }
                }
            };
            update(0.0, "Preparando audio");
            process(&core, &library, &track, &work_id, task, &work_token, update)
        });
        let result = loop {
            tokio::select! {
                result=&mut worker=>break result,
                Some((percent,message))=progress_rx.recv()=>{
                    state.notify(if matches!(task,ProcessingTask::Variant(_,"Ardilla")){format!("Creando versión ardilla de \"{notification_title}\"… {percent:.0}%")}else{format!("{message}… {percent:.0}%")});
                }
            }
        }
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
            "Completado"
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
                if matches!(task, ProcessingTask::Variant(_, "Ardilla")) {
                    format!("Lista: \"{notification_title} (Ardilla)\". Está en Canciones.")
                } else if fingerprint {
                    fingerprint_completion_summary(&output["fingerprint_verification"])
                } else {
                    "Separación completada".into()
                }
            } else {
                error
            });
        }
        state.refresh();
        state.plugin_event(
            &format!(
                "{event_prefix}.{}",
                if status == JobStatus::Completed {
                    "ready"
                } else {
                    "stopped"
                }
            ),
            serde_json::json!({"job_id":job.id,"result":output}),
        );
    });
    true
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
            let (output, extension) = if label == "Ardilla" {
                let format = chipmunk_format(track);
                if format == yt_audio::AudioFormat::Flac {
                    (output, "flac")
                } else {
                    let converted = work.path().join(format!("result.{}", format.extension()));
                    yt_audio::convert(&output, &converted, format, token, |_| {})
                        .map_err(|e| e.to_string())?;
                    (converted, format.extension())
                }
            } else {
                (output, "flac")
            };
            let result = publish(
                core,
                library,
                track,
                id,
                if label == "Ardilla" {
                    "Ardilla"
                } else {
                    "Protected"
                },
                &output,
                extension,
                token,
            )?;
            let comparison = serde_json::to_value(&report.comparison).map_err(|e| e.to_string())?;
            let verification = fingerprint_verification(&comparison);
            let profile = variant_profile(options, report.comparison.source.sample_rate);
            let metadata = if label == "Ardilla" {
                serde_json::json!({"chipmunk_profile":report.parameters})
            } else {
                serde_json::json!({"fingerprint_id":id,"fingerprint_profile":profile,"fingerprint_verification":verification})
            };
            if let Err(error) = core.merge_track_metadata(&result, &metadata) {
                rollback_generated(core, &result)?;
                return Err(error.to_string());
            }
            Ok(
                serde_json::json!({"track":result,"report":report,"fingerprint_verification":verification}),
            )
        }
        ProcessingTask::StemsFormat(_) | ProcessingTask::Quality => {
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
                if matches!(task, ProcessingTask::StemsFormat(_)) {
                    if token.is_cancelled() {
                        return Err("Procesamiento cancelado".into());
                    }
                    // Once publication starts, finish both stems together. A
                    // late cancellation must not leave a hidden partial job.
                    progress(97.0, "Preparando formato de salida");
                    let format = match task {
                        ProcessingTask::StemsFormat(format) => format,
                        _ => yt_audio::AudioFormat::Wav,
                    };
                    let extension = format.extension();
                    let final_vocals = work.path().join(format!("vocals-final.{extension}"));
                    let final_instrumental =
                        work.path().join(format!("instrumental-final.{extension}"));
                    yt_audio::convert(&vocals, &final_vocals, format, token, |_| {})
                        .map_err(|error| error.to_string())?;
                    yt_audio::convert(&instrumental, &final_instrumental, format, token, |_| {})
                        .map_err(|error| error.to_string())?;
                    progress(98.0, "Guardando vocales e instrumental");
                    if token.is_cancelled() {
                        return Err("Procesamiento cancelado".into());
                    }
                    let commit = CancellationToken::new();
                    let vocals = publish(
                        core,
                        library,
                        track,
                        id,
                        "Vocals",
                        &final_vocals,
                        extension,
                        &commit,
                    )?;
                    let instrumental = match publish(
                        core,
                        library,
                        track,
                        id,
                        "Instrumental",
                        &final_instrumental,
                        extension,
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
                let output = publish(
                    core,
                    library,
                    track,
                    id,
                    "Protected",
                    &output,
                    "flac",
                    token,
                )?;
                let verification_fingerprint = fingerprint_verification(
                    &serde_json::to_value(&comparison).map_err(|e| e.to_string())?,
                );
                let profile = quality_profile(&dsp, &verification, comparison.source.sample_rate);
                let metadata = serde_json::json!({"fingerprint_id":id,"fingerprint_profile":profile,"fingerprint_verification":verification_fingerprint});
                if let Err(error) = core.merge_track_metadata(&output, &metadata) {
                    rollback_generated(core, &output)?;
                    return Err(error.to_string());
                }
                Ok(
                    serde_json::json!({"track":output,"dsp":dsp,"verification":verification,"comparison":comparison,"fingerprint_verification":verification_fingerprint}),
                )
            }
        }
    }
}

fn chipmunk_format(track: &Track) -> yt_audio::AudioFormat {
    use yt_audio::AudioFormat;
    match track
        .extension
        .trim_start_matches('.')
        .to_ascii_lowercase()
        .as_str()
    {
        "mp3" => AudioFormat::Mp3(192),
        "wav" => AudioFormat::Wav,
        "flac" => AudioFormat::Flac,
        "ogg" => AudioFormat::Ogg(160),
        "opus" => AudioFormat::Opus(160),
        "aac" => AudioFormat::Aac(192),
        _ => AudioFormat::M4a(192),
    }
}

fn variant_profile(options: yt_audio::VariantOptions, rate: u32) -> serde_json::Value {
    let rate_hz = (rate as f64 * 2.0f64.powf(options.pitch_semitones / 12.0)).round();
    serde_json::json!({"version":3,"intensity":if options.pitch_semitones>=1.75{"strong"}else{"balanced"},"sample_rate":rate,"output_format":"FLAC","pitch_semitones":(options.pitch_semitones*100.0).round()/100.0,"speed":(options.speed*1000.0).round()/1000.0,"rate_hz":rate_hz as u32,"tempo_ratio":(options.speed/(rate_hz/rate as f64)*10_000_000.0).round()/10_000_000.0,"low_gain_db":(options.low_gain_db*1000.0).round()/1000.0,"high_gain_db":(options.high_gain_db*1000.0).round()/1000.0,"engine":"rust-soundtouch"})
}

#[cfg(feature = "native-stems")]
fn quality_profile(
    dsp: &yt_quality::QualityReport,
    audit: &yt_quality::VerificationReport,
    source_rate: u32,
) -> serde_json::Value {
    serde_json::json!({
        "version":6,"intensity":"quality","sample_rate":44100,"source_sample_rate":source_rate,
        "output_format":"FLAC","pitch_semitones":0.28,"pitch_range_semitones":[0.20,0.36],
        "speed":dsp.shared.mean_speed_ratio,"rate_hz":44100,"tempo_ratio":dsp.shared.mean_speed_ratio,
        "instrumental_method":"dynamic_spectral_eq","max_spectral_gain_db":6.0,
        "processing_target":"instrumental_then_full_mix","vocals_preserved":false,"vocal_processing":"shared_smooth_remap",
        "separator_model":"BS-RoFormer viperx SDR 12.9755","engine":"rust-soxr",
        "instrumental_processing":dsp.instrumental,"instrumental_gain":dsp.mix.instrumental_gain,
        "shared_processing":dsp.shared,"dsp":dsp,
        "pre_remap_vocal_integrity":{"status":"verified","scope":"separated_stem_before_shared_remap","frames":dsp.shared.input_frames,"channels":2,"sample_rate":44100,"max_abs_error":dsp.pre_remap_pcm24_error,"tolerance":2.0f64.powi(-23),"bit_depth":24,"reference_format":"PCM24 values in WAV"},
        "final_audio_integrity":{"status":"verified","scope":"processed_full_mix","frames":audit.frames,"sample_rate":audit.sample_rate,"channels":audit.channels,"max_abs_error":audit.max_abs_error,"tolerance":audit.tolerance,"output_format":"FLAC","bit_depth":audit.bit_depth,"true_peak_method":"fir_polyphase_8x_kaiser5","true_peak_estimate_dbtp":audit.true_peak_estimate_dbtp,"max_allowed_true_peak_dbtp":-1.0}
    })
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
    let group = match label {
        "Vocals" | "Vocales" | "Instrumental" => "Separaciones",
        "Ardilla" => "Ardillas",
        _ => "Protegidos",
    };
    let folder = library.join(group);
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let folder = folder.canonicalize().map_err(|e| e.to_string())?;
    if !folder.starts_with(library.canonicalize().map_err(|e| e.to_string())?) {
        return Err("La carpeta de resultados sale de la biblioteca".into());
    }
    let title = if label == "Ardilla" {
        format!("{} (Ardilla)", track.title)
    } else {
        format!("{} - {label}", track.title)
    };
    let destination = folder.join(format!(
        "{id}-{label}-{}",
        yt_media::safe_file_name(&title, extension)
    ));
    let mut file = tempfile::Builder::new()
        .prefix(".ytu-publish-")
        .suffix(&format!(".{extension}"))
        .tempfile_in(&folder)
        .map_err(|e| e.to_string())?;
    let mut reader = std::fs::File::open(source).map_err(|e| e.to_string())?;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        if token.is_cancelled() {
            return Err("Procesamiento cancelado".into());
        }
        let count = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        file.write_all(&buffer[..count])
            .map_err(|e| e.to_string())?;
    }
    if core
        .settings()
        .map_err(|error| error.to_string())?
        .preserve_source_metadata
    {
        let mut metadata = core
            .media_metadata(&track.id)
            .map_err(|error| error.to_string())?;
        metadata.title = Some(title.clone());
        yt_audio::copy_metadata(&track.path, file.path(), &metadata, token)
            .map_err(|error| error.to_string())?;
    }
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
    let metadata = serde_json::json!({"playlist":group,"source_file":track.filename,"quality":extension.to_uppercase(),"separator_model":if group=="Separaciones"{"BS-RoFormer viperx SDR 12.9755"}else{""},"stem":match label{"Vocals"|"Vocales"=>"vocals","Instrumental"=>"instrumental",_=>""}});
    if let Err(error) = core.merge_track_metadata(&imported.id, &metadata) {
        rollback_generated(core, &imported.id)?;
        return Err(error.to_string());
    }
    Ok(imported.id)
}

pub(crate) fn fingerprint_verification(comparison: &serde_json::Value) -> serde_json::Value {
    let before = comparison["source"]["frames"].as_array();
    let after = comparison["output"]["frames"].as_array();
    let status = match (before, after) {
        (Some(before), Some(after)) if before.len().min(after.len()) < 16 => "insufficient_audio",
        (Some(before), Some(after)) if before.iter().zip(after).any(|(a, b)| a != b) => "changed",
        (Some(_), Some(_)) => "unchanged",
        _ => "unavailable",
    };
    let mut result = serde_json::json!({"status":status,"algorithm":"chromaprint-2","max_analysis_seconds":120,"pcm_algorithm":"native_rate_pcm16le_with_layout_v1","pcm_changed":comparison["pcm_changed"],"source_pcm_sha256":comparison["source"]["pcm_sha256"],"output_pcm_sha256":comparison["output"]["pcm_sha256"],"recognition_tested":comparison["recognition_tested"],"source_frames":before.map_or(0,Vec::len),"output_frames":after.map_or(0,Vec::len),"local_comparison":comparison["local_comparison"]});
    if matches!(status, "changed" | "unchanged") {
        result["source_fingerprint_sha256"] = comparison["source"]["fingerprint_sha256"].clone();
        result["output_fingerprint_sha256"] = comparison["output"]["fingerprint_sha256"].clone();
    }
    result
}
pub(crate) fn fingerprint_summary(verification: &serde_json::Value) -> &'static str {
    match verification["status"].as_str() {
        Some("changed") => "Huella Chromaprint distinta en el fragmento analizado (hasta 120 s).",
        Some("unchanged") => {
            "Audio modificado; la huella Chromaprint sigue igual en el fragmento analizado."
        }
        Some("insufficient_audio") => {
            "Audio modificado; el fragmento es demasiado corto para comparar huellas."
        }
        Some("unavailable") => {
            "Audio modificado; falta Chromaprint para verificar la huella acústica."
        }
        _ => "Huella acústica sin verificar.",
    }
}
fn fingerprint_completion_summary(verification: &serde_json::Value) -> String {
    format!(
        "Variación creada. {} {}",
        fingerprint_summary(verification),
        if verification["recognition_tested"] == true {
            "Prueba externa registrada; consulta el resultado del detector."
        } else {
            "Reconocimiento externo sin comprobar."
        }
    )
}

fn rollback_generated(core: &AppCore, id: &str) -> Result<(), String> {
    let path = core.track(id).map_err(|e| e.to_string())?.path;
    // Only called for a new file published by this operation with no-clobber.
    core.remove_track(id).map_err(|e| e.to_string())?;
    std::fs::remove_file(path)
        .map_err(|e| format!("No se pudo retirar el resultado incompleto: {e}"))
}

#[cfg(feature = "native-stems")]
pub(crate) fn model_path() -> Result<std::path::PathBuf, String> {
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
    fn fingerprint_result_keeps_persistent_profile_and_original_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let core = AppCore::open(temp.path()).unwrap();
        let bytes = include_bytes!("../../yt-quality/tests/fixtures/input.wav");
        let track = core.import_bytes("Original.wav", bytes).unwrap();
        let result = process(
            &core,
            core.library_dir(),
            &track,
            "fingerprint-job",
            ProcessingTask::Variant(
                yt_audio::VariantOptions::variation("fingerprint-job", true),
                "Intenso",
            ),
            &CancellationToken::new(),
            |_, _| {},
        )
        .unwrap();
        let output = core.track(result["track"].as_str().unwrap()).unwrap();
        assert_eq!(
            output.path.parent().unwrap(),
            core.library_dir().join("Protegidos")
        );
        assert_eq!(output.title, "Original - Protected");
        let metadata = core.track_metadata(&output.id).unwrap();
        assert_eq!(metadata["playlist"], "Protegidos");
        assert_eq!(metadata["fingerprint_profile"]["intensity"], "strong");
        assert_eq!(metadata["fingerprint_profile"]["sample_rate"], 44100);
        assert!(metadata["fingerprint_profile"]["rate_hz"].as_u64().unwrap() > 44100);
        assert!(
            metadata["fingerprint_profile"]["tempo_ratio"]
                .as_f64()
                .unwrap()
                < 1.0
        );
        assert_eq!(
            metadata["fingerprint_verification"]["status"],
            "insufficient_audio"
        );
        assert_eq!(
            metadata["fingerprint_verification"]["recognition_tested"],
            false
        );
        assert_eq!(
            metadata["fingerprint_verification"]["max_analysis_seconds"],
            120
        );
        assert_eq!(
            metadata["fingerprint_verification"]["source_pcm_sha256"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        assert_eq!(
            metadata["fingerprint_verification"]["pcm_algorithm"],
            "native_rate_pcm16le_with_layout_v1"
        );
        assert!(core.jobs().unwrap().is_empty());
        assert_eq!(std::fs::read(&track.path).unwrap(), bytes);
        assert!(
            fingerprint_completion_summary(&result["fingerprint_verification"])
                .contains("Reconocimiento externo sin comprobar.")
        );
    }
    #[cfg(feature = "native-stems")]
    #[test]
    fn quality_metadata_keeps_verified_final_and_pre_remap_audits() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.wav");
        std::fs::write(
            &source,
            include_bytes!("../../yt-quality/tests/fixtures/input.wav"),
        )
        .unwrap();
        let token = CancellationToken::new();
        let prepared = dir.path().join("prepared.wav");
        yt_quality::prepare_source(&source, &prepared, &token).unwrap();
        let reference = dir.path().join("reference.wav");
        let dsp = yt_quality::process_stems(
            &prepared,
            &prepared,
            &reference,
            "profile-test",
            &token,
            |_| {},
        )
        .unwrap();
        let flac = dir.path().join("final.flac");
        yt_audio::convert(
            &reference,
            &flac,
            yt_audio::AudioFormat::Flac,
            &token,
            |_| {},
        )
        .unwrap();
        let audit = yt_quality::verify_final_flac(&reference, &flac, &token).unwrap();
        let profile = quality_profile(&dsp, &audit, 44100);
        assert_eq!(
            profile["shared_processing"]["mean_speed_ratio"],
            profile["speed"]
        );
        assert_eq!(
            profile["pre_remap_vocal_integrity"]["scope"],
            "separated_stem_before_shared_remap"
        );
        assert_eq!(profile["final_audio_integrity"]["status"], "verified");
        assert_eq!(profile["final_audio_integrity"]["frames"], audit.frames);
        assert_eq!(
            profile["final_audio_integrity"]["true_peak_method"],
            "fir_polyphase_8x_kaiser5"
        );
        assert!(
            profile["final_audio_integrity"]["max_abs_error"]
                .as_f64()
                .unwrap()
                <= 2.0f64.powi(-23)
        );
    }

    #[test]
    fn chipmunk_keeps_wav_format_and_publishes_legacy_folder_and_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let core = AppCore::open(temp.path()).unwrap();
        let track = core
            .import_bytes(
                "Original.wav",
                include_bytes!("../../yt-quality/tests/fixtures/input.wav"),
            )
            .unwrap();
        let result = process(
            &core,
            core.library_dir(),
            &track,
            "chipmunk-job",
            ProcessingTask::Variant(yt_audio::VariantOptions::chipmunk(7.0, 1.0, 3.0), "Ardilla"),
            &CancellationToken::new(),
            |_, _| {},
        )
        .unwrap();
        let output = core.track(result["track"].as_str().unwrap()).unwrap();
        assert_eq!(output.extension, "wav");
        assert_eq!(
            output.path.parent().unwrap(),
            core.library_dir().join("Ardillas")
        );
        assert_eq!(output.title, "Original (Ardilla)");
        let metadata = core.track_metadata(&output.id).unwrap();
        assert_eq!(metadata["playlist"], "Ardillas");
        assert_eq!(metadata["chipmunk_profile"]["pitch_semitones"], 7.0);
        assert!(metadata["fingerprint_profile"].is_null());
    }
    #[test]
    fn chipmunk_keeps_additional_formats_and_embedded_cover_through_publication() {
        let temp = tempfile::tempdir().unwrap();
        let core = AppCore::open(temp.path()).unwrap();
        let source = core
            .import_bytes(
                "Original.wav",
                include_bytes!("../../yt-quality/tests/fixtures/input.wav"),
            )
            .unwrap();
        let artwork = core
            .cache_artwork(include_bytes!("../../yt-core/tests/fixtures/cover.png"))
            .unwrap();
        for format in [
            yt_audio::AudioFormat::M4a(192),
            yt_audio::AudioFormat::Aac(192),
            yt_audio::AudioFormat::Ogg(160),
            yt_audio::AudioFormat::Opus(160),
        ] {
            let input = temp.path().join(format!("Original.{}", format.extension()));
            yt_audio::convert_with_metadata(
                &source.path,
                &input,
                format,
                &yt_audio::MediaMetadata {
                    title: Some("Original".into()),
                    artwork_path: Some(artwork.clone()),
                    ..Default::default()
                },
                &CancellationToken::new(),
                |_| {},
            )
            .unwrap();
            let track = core.import_file(&input, yt_core::ImportMode::Copy).unwrap();
            let original = std::fs::read(&track.path).unwrap();
            let result = process(
                &core,
                core.library_dir(),
                &track,
                &format!("job-{}", format.extension()),
                ProcessingTask::Variant(
                    yt_audio::VariantOptions::chipmunk(7.0, 1.0, 3.0),
                    "Ardilla",
                ),
                &CancellationToken::new(),
                |_, _| {},
            )
            .unwrap();
            let output = core.track(result["track"].as_str().unwrap()).unwrap();
            assert_eq!(output.extension, format.extension());
            assert!(core
                .media_metadata(&output.id)
                .unwrap()
                .artwork_path
                .is_some());
            assert_eq!(std::fs::read(&track.path).unwrap(), original);
            assert_eq!(
                core.track_metadata(&output.id).unwrap()["chipmunk_profile"]["brightness_db"],
                3.0
            );
        }
    }
    #[test]
    fn generated_stems_with_long_unicode_titles_have_distinct_names() {
        let temp = tempfile::tempdir().unwrap();
        let core = AppCore::open(temp.path()).unwrap();
        let mut track = core
            .import_bytes(
                "source.wav",
                include_bytes!("../../yt-quality/tests/fixtures/input.wav"),
            )
            .unwrap();
        track.title = "界".repeat(100);
        let original = std::fs::read(&track.path).unwrap();
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
        rollback_generated(&core, &vocals).unwrap();
        rollback_generated(&core, &music).unwrap();
        assert_eq!(core.library("").unwrap().len(), 1);
        assert_eq!(std::fs::read(&track.path).unwrap(), original);
    }
}
