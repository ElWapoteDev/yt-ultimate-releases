use crate::{
    app::{artwork_src, EmptyState, MediaCard, Thumb, TrackRow},
    icons::Icon,
    state::{size_label, time_label, AppState},
};
use dioxus::prelude::*;
use yt_core::{Job, Track};
use yt_media::CancellationToken;

#[component]
pub fn LibraryPage() -> Element {
    let s = use_context::<AppState>();
    let mut query = use_signal(String::new);
    let mut filter = use_signal(|| "all");
    let mut grid = use_signal(|| true);
    let tracks = filtered_tracks(&s.library.read(), &query.read(), *filter.read(), false);
    let play_all = tracks.clone();
    let shuffled = tracks.clone();
    let play_state = s.clone();
    let shuffle_state = s.clone();
    let import_state = s.clone();
    let empty_import_state = s.clone();
    let device = if cfg!(target_os = "windows") {
        "tu PC"
    } else {
        "tu Mac"
    };
    rsx! { div { class: "page-shell",
        div { class: "toolbar",
            div { class: "toolbar__left", h1 { class: "page-title", "Canciones" } span { class: "page-count", "{tracks.len()}" } }
            div { class: "toolbar__right",
                if !tracks.is_empty() {
                    button { class: "btn-accent",title:"Reproducir en orden", onclick: move |_| play_state.play_all(play_all.clone(), false), Icon { name: "play", size: 13, fill: "currentColor" } "Reproducir" }
                    button { class: "btn-secondary", title: "Reproducir en orden aleatorio", onclick: move |_| shuffle_state.play_all(shuffled.clone(), true), Icon { name: "shuffle", size: 13 } }
                }
                button { class: "btn-secondary", title:"Importar archivos o carpetas",disabled: *s.busy.read(), onclick: move |_| import_state.import_files(false),FilePlusIcon {size:13}"Importar" }
                div { class: "segmented", for (id, label) in [("all", "Todo"), ("audio", "Audio"), ("video", "Vídeo")] {
                    button { class: if *filter.read() == id { "segment active" } else { "segment" }, onclick: move |_| filter.set(id), "{label}" }
                } }
                button { class: "btn-icon", title: if *grid.read(){"Ver como lista"}else{"Ver como cuadrícula"}, onclick: move |_| grid.toggle(), Icon { name: if *grid.read() { "list" } else { "grid" },size:15 } }
            }
        }
        if !s.library.read().is_empty() {
            div { class: "input-search-wrap", style: "max-width:360px;margin-bottom:22px",
                span { class:"input-search-icon", Icon { name:"search", size:14 } }
                input { class: "input-search", placeholder: "Filtrar canciones", aria_label: "Filtrar canciones", value: "{query}", oninput: move |e| query.set(e.value()) }
                if !query.read().is_empty() { button { class:"input-search-clear", aria_label:"Limpiar", onclick:move |_| query.set(String::new()), "×" } }
            }
        }
        if *s.busy.read() { p { class: "rust-loading", role: "status", "Procesando archivos…" } }
        if s.library.read().is_empty() {
            EmptyState { title: "Aún no hay música", hint:format!("Descarga desde Buscar o añade música que ya tengas en {device}."),
                button { class: "btn-accent", onclick: move |_| empty_import_state.import_files(false), FilePlusIcon {size:14} "Importar música" }
            }
        } else if tracks.is_empty() {
            EmptyState { title: "Nada coincide", hint: "Prueba con otro término o cambia el filtro.", icon: "search",compact:true }
        } else if *grid.read() {
            div { class: "media-grid", for track in tracks { MediaCard { key: "{track.id}", track } } }
        } else {
            for (index, track) in tracks.into_iter().enumerate() { TrackRow { key: "{track.id}", track, index } }
        }
    } }
}

#[component]
fn FilePlusIcon(size: u32) -> Element {
    rsx! {svg{width:size,height:size,view_box:"0 0 24 24",fill:"none",stroke:"currentColor",stroke_width:"1.8",stroke_linecap:"round",stroke_linejoin:"round","aria-hidden":"true",path{d:"M11.35 22H6a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.706.706l3.588 3.588A2.4 2.4 0 0 1 20 8v5.35"}path{d:"M14 2v5a1 1 0 0 0 1 1h5"}path{d:"M14 19h6"}path{d:"M17 16v6"}}}
}

#[component]
pub fn InformationDialog(track: Track, onclose: EventHandler<()>) -> Element {
    let state = use_context::<AppState>();
    let mut fingerprint = use_signal(|| None::<Result<yt_audio::AudioFingerprint, String>>);
    let mut loading = use_signal(|| false);
    let mut cancellation = use_signal(|| None::<CancellationToken>);
    use_drop(move || {
        if let Some(token) = cancellation.peek().as_ref() {
            token.cancel();
        }
    });
    let report = use_hook(|| {
        state
            .services
            .core
            .jobs()
            .unwrap_or_default()
            .into_iter()
            .find(|job| {
                job.status == yt_core::JobStatus::Completed && job.result["track"] == track.id
            })
    });
    let stored = use_hook(|| {
        state
            .services
            .core
            .track_metadata(&track.id)
            .unwrap_or_default()
    });
    let details = information_rows(&track, report.as_ref(), &stored);
    let source = track.path.clone();
    let can_analyze = !track.missing && !track.path.as_os_str().is_empty();
    let analyze = move |_| {
        if *loading.peek() {
            return;
        }
        let token = CancellationToken::new();
        cancellation.set(Some(token.clone()));
        loading.set(true);
        fingerprint.set(None);
        let path = source.clone();
        spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                yt_audio::fingerprint(&path, &token).map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| value);
            fingerprint.set(Some(result));
            loading.set(false);
            cancellation.set(None);
        });
    };
    rsx! {
        style {{include_str!("library_ui.css")}}
        div {class:"modal-backdrop",onclick:move |_|onclose.call(()),
            div {class:"modal-panel library-information",role:"dialog",aria_modal:"true",aria_label:"Información de la canción",onclick:|event|event.stop_propagation(),onkeydown:move |event|{if event.key()==Key::Escape{event.stop_propagation();onclose.call(());}},
                div {class:"modal-header",div {class:"library-info-heading",Thumb {src:artwork_src(&track),size:"lg",icon_size:Some(20)}div {class:"library-info-meta",div {class:"modal-title","{track.title}"}div {class:"library-info-artist",if track.artist.is_empty(){"Desconocido"}else{"{track.artist}"}}}}
                    button {class:"btn-icon-sm",title:"Cerrar",aria_label:"Cerrar información",onmounted:|event|async move{let _=event.set_focus(true).await;},onclick:move |_|onclose.call(()),Icon {name:"close",size:14}}
                }
                for (label,value) in details {div {class:"info-row",span {class:"info-row__label","{label}"}span {class:"info-row__value","{value}"}}}
                div {class:"library-fingerprint-actions",
                    button {class:"btn-secondary",disabled:!can_analyze||*loading.read(),onclick:analyze,if *loading.read(){"Calculando huella…"}else{"Calcular huella local"}}
                    if *loading.read(){button {class:"btn-icon-sm",title:"Cancelar cálculo",onclick:move |_|{let token=cancellation.peek().clone();if let Some(token)=token{token.cancel();}},Icon{name:"close",size:13}}}
                }
                if let Some(result)=fingerprint.read().as_ref(){
                    match result {
                        Ok(value)=>rsx!{
                            div {class:"info-row",span {class:"info-row__label","Audio"}span {class:"info-row__value","{value.sample_rate} Hz · {value.channels} canales"}}
                            div {class:"info-row",span {class:"info-row__label","SHA-256 del audio"}span {class:"info-row__value library-fingerprint-hash","{value.pcm_sha256}"}}
                            div {class:"info-row",span {class:"info-row__label","Huella acústica"}span {class:"info-row__value",if value.frames.is_empty(){"Fragmento demasiado corto para obtener subhuellas Chromaprint."}else{"{value.frames.len()} subhuellas Chromaprint · hasta 120 segundos"}}}
                            p {class:"library-fingerprint-note",role:"status","Cálculo local. No se consultó ningún servicio de reconocimiento."}
                        },
                        Err(error)=>rsx!{p {class:"task-row__error",role:"status","{error}"}},
                    }
                }
            }
        }
    }
}

fn filtered_tracks(tracks: &[Track], query: &str, filter: &str, favorites: bool) -> Vec<Track> {
    let query = query.trim().to_lowercase();
    tracks
        .iter()
        .filter(|track| {
            (filter == "all" || track.media_type == filter) && (!favorites || track.favorite)
        })
        .filter(|track| {
            query.is_empty()
                || format!(
                    "{} {} {} {}",
                    track.title, track.artist, track.album, track.filename
                )
                .to_lowercase()
                .contains(&query)
        })
        .cloned()
        .collect()
}
fn information_rows(
    track: &Track,
    job: Option<&Job>,
    stored: &serde_json::Value,
) -> Vec<(String, String)> {
    let mut rows = vec![
        ("Archivo".into(), track.filename.clone()),
        (
            "Tipo".into(),
            if track.media_type == "video" {
                "Vídeo"
            } else {
                "Audio"
            }
            .into(),
        ),
        ("Tamaño".into(), size_label(track.size_bytes)),
        ("Duración".into(), time_label(track.duration)),
        (
            "Ruta".into(),
            if track.path.as_os_str().is_empty() {
                "En línea".into()
            } else {
                track.path.display().to_string()
            },
        ),
    ];
    let fallback = job.map(|job| {
        let comparison=if job.result["comparison"].is_object(){&job.result["comparison"]}else{&job.result["report"]["comparison"]};
        let parameters=&job.result["report"]["parameters"];
        let profile=if job.kind=="quality"{serde_json::json!({"version":6,"intensity":"quality"})}
        else if job.kind == "variant" && parameters.is_object(){let mut profile=parameters.clone();profile["intensity"]=serde_json::json!(if parameters["pitch_semitones"].as_f64().unwrap_or(0.0)>=1.75{"strong"}else{"balanced"});profile}
        else{serde_json::Value::Null};
        serde_json::json!({"fingerprint_profile":profile,"fingerprint_verification":if comparison.is_object(){crate::processing::fingerprint_verification(comparison)}else{serde_json::Value::Null}})
    }).unwrap_or_default();
    let profile = if stored["fingerprint_profile"].is_object() {
        &stored["fingerprint_profile"]
    } else {
        &fallback["fingerprint_profile"]
    };
    if profile.is_object() {
        let quality = profile["intensity"] == "quality";
        let version = profile["version"].as_u64().unwrap_or_default();
        rows.push((
            "Variación".into(),
            if quality {
                if version >= 6 {
                    "CALIDAD · cambios suaves en voz e instrumental"
                } else {
                    "CALIDAD · cambios solo en la instrumental"
                }
                .into()
            } else {
                format!(
                    "{} · +{} semitonos · {}×",
                    if profile["intensity"] == "strong" {
                        "Intensa"
                    } else {
                        "Equilibrada"
                    },
                    number_text(&profile["pitch_semitones"]),
                    number_text(&profile["speed"])
                )
            },
        ));
        if quality || profile["vocals_preserved"] == true {
            rows.push(("Voz".into(), vocal_summary(profile).into()));
        }
    }
    let verification = if stored["fingerprint_verification"].is_object() {
        &stored["fingerprint_verification"]
    } else {
        &fallback["fingerprint_verification"]
    };
    if verification.is_object() {
        rows.push(("Fingerprint".into(),format!("{} Esto no garantiza que otros sistemas de reconocimiento no identifiquen el audio.",crate::processing::fingerprint_summary(verification))));
        let comparison = &verification["local_comparison"];
        let aligned = if comparison["best_offset"].is_object() {
            &comparison["best_offset"]
        } else {
            &comparison["aligned"]
        };
        if comparison["metric"] == "chromaprint_bit_agreement_v1" {
            if let (Some(similarity), Some(frames)) = (
                aligned["bit_similarity"].as_f64(),
                aligned["compared_frames"].as_u64(),
            ) {
                if similarity.is_finite() && (0.0..=1.0).contains(&similarity) && frames >= 16 {
                    rows.push(("Coincidencia local de bits".into(),format!("{} % · {frames} subhuellas Chromaprint, con alineación local. No mide si un detector reconocerá la canción.",format!("{:.1}",similarity*100.0).replace('.',","))));
                }
            }
        }
        rows.push((
            "Reconocimiento externo".into(),
            if verification["recognition_tested"] == true {
                "Prueba externa registrada; consulta el resultado del detector."
            } else {
                "Sin comprobar."
            }
            .into(),
        ));
    }
    for (_, value) in &mut rows {
        if value.is_empty() {
            *value = "—".into();
        }
    }
    rows
}
fn number_text(value: &serde_json::Value) -> String {
    value
        .as_f64()
        .filter(|n| n.is_finite())
        .map(|value| value.to_string())
        .unwrap_or_else(|| "—".into())
}
fn vocal_summary(profile: &serde_json::Value) -> &'static str {
    if profile["intensity"] == "quality" && profile["version"].as_u64().unwrap_or_default() >= 6 {
        return "Voz e instrumental reciben juntas cambios suaves de tono, velocidad y volumen. La calidad se valora al escuchar cada resultado.";
    }
    let integrity = &profile["vocal_integrity"];
    let verified = integrity["status"] == "verified"
        && integrity["scope"] == "separated_stem"
        && integrity["frames"].as_u64().is_some_and(|n| n > 0)
        && integrity["channels"].as_u64().is_some_and(|n| n > 0)
        && integrity["tolerance"]
            .as_f64()
            .is_some_and(|n| n > 0.0 && n <= 2.0f64.powi(-23))
        && integrity["max_abs_error"]
            .as_f64()
            .is_some_and(|n| n >= 0.0 && n <= integrity["tolerance"].as_f64().unwrap_or(-1.0));
    if verified {
        "Voz separada comprobada en el FLAC final, dentro de la precisión de 24 bits. La separación por IA puede dejar artefactos."
    } else if profile["vocals_preserved"] == true {
        "Voz separada sin efectos; conservación de muestras en la mezcla final sin verificar."
    } else {
        "Conservación de la voz sin verificar."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{use_app_state, Services};
    use std::rc::Rc;

    fn harness() -> Element {
        let services = use_context::<Rc<Services>>();
        let state = use_app_state(services);
        let track = state.library.read().first().cloned().unwrap();
        use_context_provider(|| state);
        rsx! { LibraryPage {} InformationDialog { track, onclose: |_| {} } }
    }

    #[tokio::test]
    async fn library_and_information_render_actual_file_details_and_local_analysis() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let track = services
            .core
            .import_bytes(
                "Canción.wav",
                include_bytes!("../../yt-quality/tests/fixtures/input.wav"),
            )
            .unwrap();
        let mut dom = VirtualDom::new(harness);
        dom.insert_any_root_context(Box::new(services));
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        for text in [
            "Canciones",
            "Filtrar canciones",
            "input-search-icon",
            "Todo",
            "Audio",
            "Vídeo",
            "Ver como lista",
            "media-grid",
            "Información de la canción",
            "Archivo",
            "Tamaño",
            "Duración",
            "Ruta",
            "Calcular huella local",
            &track.filename,
            track.path.to_str().unwrap(),
        ] {
            assert!(html.contains(text), "missing library UI: {text}");
        }
        assert!(!html.contains("Coincidencia local de bits"));
        assert!(!html.contains("SHA-256 del audio"));
    }

    #[test]
    fn library_filters_include_filename_album_type_and_favorites_together() {
        let profile = tempfile::tempdir().unwrap();
        let core = yt_core::AppCore::open(profile.path()).unwrap();
        let mut audio = core.import_bytes("Árbol.wav", b"audio fixture").unwrap();
        audio.title = "Pista".into();
        audio.album = "Álbum especial".into();
        audio.favorite = true;
        let mut video = audio.clone();
        video.id = "video".into();
        video.media_type = "video".into();
        video.favorite = false;
        let tracks = vec![audio.clone(), video];
        assert_eq!(
            filtered_tracks(&tracks, " ÁRBOL ", "audio", true),
            vec![audio.clone()]
        );
        assert_eq!(filtered_tracks(&tracks, "ÁLBUM", "all", true), vec![audio]);
        assert_eq!(filtered_tracks(&tracks, "pista", "video", false).len(), 1);
        assert!(filtered_tracks(&tracks, "pista", "video", true).is_empty());
        assert!(filtered_tracks(&tracks, "missing", "all", false).is_empty());
    }

    #[test]
    fn processing_information_distinguishes_local_evidence_from_recognition() {
        let profile = tempfile::tempdir().unwrap();
        let core = yt_core::AppCore::open(profile.path()).unwrap();
        let track = core.import_bytes("variant.flac", b"audio fixture").unwrap();
        let mut job = core.create_job("variant", &serde_json::json!({})).unwrap();
        job.result = serde_json::json!({"report":{"parameters":{"pitch_semitones":7.0,"speed":1.25},"comparison":{"pcm_changed":true,"recognition_tested":false,"source":{"frames":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1]},"output":{"frames":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2]},"local_comparison":{"metric":"chromaprint_bit_agreement_v1","best_offset":{"bit_similarity":0.875,"compared_frames":32}}}}});
        let rows = information_rows(&track, Some(&job), &serde_json::json!({}));
        assert!(rows
            .iter()
            .any(|(_, value)| value == "Intensa · +7 semitonos · 1.25×"));
        assert!(rows
            .iter()
            .any(|(_, value)| value.starts_with("87,5 % · 32")));
        assert!(rows
            .iter()
            .any(|(_, value)| value.starts_with("Huella Chromaprint distinta")));
        assert!(rows
            .iter()
            .any(|(label, value)| label == "Reconocimiento externo"
                && value.starts_with("Sin comprobar.")));
        job.result["report"]["comparison"]["local_comparison"]["best_offset"]["compared_frames"] =
            serde_json::json!(3);
        let rows = information_rows(&track, Some(&job), &serde_json::json!({}));
        assert!(!rows
            .iter()
            .any(|(label, _)| label == "Coincidencia local de bits"));
        assert!(rows
            .iter()
            .any(|(_, value)| value.starts_with("Huella Chromaprint distinta")));
    }

    #[test]
    fn legacy_metadata_information_does_not_require_retained_job_history() {
        let temp = tempfile::tempdir().unwrap();
        let core = yt_core::AppCore::open(temp.path()).unwrap();
        let track = core.import_bytes("Legacy.flac", b"fixture").unwrap();
        core.merge_track_metadata(&track.id,&serde_json::json!({"fingerprint_profile":{"version":6,"intensity":"quality"},"fingerprint_verification":{"status":"changed","recognition_tested":false,"local_comparison":{"metric":"chromaprint_bit_agreement_v1","best_offset":{"bit_similarity":0.75,"compared_frames":40}}}})).unwrap();
        let rows = information_rows(&track, None, &core.track_metadata(&track.id).unwrap());
        assert!(rows.iter().any(|(label, value)| label == "Variación"
            && value == "CALIDAD · cambios suaves en voz e instrumental"));
        assert!(rows.iter().any(|(label, value)| label == "Voz"
            && value.starts_with("Voz e instrumental reciben juntas")));
        assert!(rows
            .iter()
            .any(|(label, value)| label == "Coincidencia local de bits"
                && value.starts_with("75,0 % · 40")));
        assert!(rows
            .iter()
            .any(|(label, value)| label == "Reconocimiento externo" && value == "Sin comprobar."));
    }
}
