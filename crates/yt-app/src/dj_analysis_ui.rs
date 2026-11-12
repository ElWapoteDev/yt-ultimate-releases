//! Developer view of the DJ's analysis of the playing song: tempo, meter, key,
//! loudness, sections, the energy of every bar and how loud its vocals are,
//! to check the analysis by ear. Shown in Settings while developer mode is
//! on. See `docs/systems/Dj.md`.

use crate::{
    dj_analysis::AnalysisStatus,
    settings_ui::{SectionHeader, Stat},
    state::AppState,
};
use dioxus::prelude::*;
use yt_core::{BandEnergy, KeyEstimate, KeyMode, Meter, Subdivision, TrackAnalysis};
use yt_worker::{protocol::DeferReason, Device};

/// Columns in each energy strip; longer songs show the loudest bar of each
/// group of bars.
const STRIP_COLUMNS: usize = 96;
/// Band level drawn as an empty column, in dB.
const STRIP_FLOOR_DB: f32 = -60.0;
/// Tonic names in Spanish, by pitch class from C, for major and minor keys.
const MAJOR_TONICS: [&str; 12] = [
    "Do", "Re♭", "Re", "Mi♭", "Mi", "Fa", "Fa♯", "Sol", "La♭", "La", "Si♭", "Si",
];
const MINOR_TONICS: [&str; 12] = [
    "Do", "Do♯", "Re", "Mi♭", "Mi", "Fa", "Fa♯", "Sol", "Sol♯", "La", "Si♭", "Si",
];

#[component]
pub(crate) fn DjAnalysisPanel() -> Element {
    let state = use_context::<AppState>();
    let dj_mode = state.settings.read().playback.dj_mode;
    let current = state.current.read().clone();
    let shown = state.dj_analysis.shown.read().clone().filter(|shown| {
        current
            .as_ref()
            .is_some_and(|track| track.id == shown.track_id)
    });
    let message = match (&current, &shown) {
        _ if !dj_mode => Some(("Activa el modo DJ para que analice las canciones.", None)),
        (None, _) => Some(("No suena nada.", None)),
        (Some(track), None) if track.media_type == "video" => {
            Some(("Los vídeos no se analizan.", None))
        }
        (Some(track), None) if crate::remote::is_remote(track) => {
            Some(("Se analiza cuando termina de descargarse.", None))
        }
        (Some(_), None) => Some(("En espera.", None)),
        (Some(_), Some(shown)) => status_message(&shown.status),
    };
    let ready = shown.and_then(|shown| match shown.status {
        AnalysisStatus::Ready { analysis, device } => Some((analysis, device)),
        _ => None,
    });
    rsx! {
        style { {include_str!("dj_analysis_ui.css")} }
        section {
            class: "settings-section dj-analysis",
            SectionHeader {
                icon: "activity",
                title: "Análisis del DJ",
                description: "Lo que el DJ mide de la canción que suena",
            }
            if let Some((title, detail)) = message {
                div {
                    class: "dj-analysis__message",
                    span {
                        class: "settings-row__title",
                        "{title}"
                    }
                    if let Some(detail) = detail {
                        span {
                            class: "settings-row__sub",
                            "{detail}"
                        }
                    }
                }
            }
            if let Some((analysis, device)) = ready {
                AnalysisDetails {
                    analysis: (*analysis).clone(),
                    device,
                }
            }
        }
    }
}

#[component]
fn AnalysisDetails(analysis: TrackAnalysis, device: Option<Device>) -> Element {
    let stats = [
        ("Tempo", tempo_label(analysis.tempo_bpm)),
        ("Estabilidad", percent(analysis.tempo_stability)),
        ("Compás", meter_label(analysis.meter)),
        ("Tonalidad", key_label(analysis.key)),
        ("Sonoridad", loudness_label(analysis.integrated_lufs)),
        (
            "Pico real",
            format!("{} dBTP", decimal(analysis.true_peak_dbtp.into(), 1)),
        ),
        ("Pulsos", analysis.beats.len().to_string()),
        ("Compases", analysis.downbeats.len().to_string()),
        ("Secciones", analysis.sections.len().to_string()),
        (
            "Voz",
            if analysis.vocal_db.is_empty() {
                "Sin medir"
            } else {
                "Medida"
            }
            .to_owned(),
        ),
        ("Calculado en", device_label(device).to_owned()),
    ];
    let band = |level: fn(&BandEnergy) -> f32| {
        strip(&analysis.bar_energy.iter().map(level).collect::<Vec<_>>())
    };
    let mut strips = vec![
        ("Graves", band(|band| band.low_db)),
        ("Medios", band(|band| band.mid_db)),
        ("Agudos", band(|band| band.high_db)),
    ];
    if !analysis.vocal_db.is_empty() {
        strips.push(("Voz", strip(&analysis.vocal_db)));
    }
    rsx! {
        div {
            class: "settings-stat-grid",
            for (label, value) in stats {
                Stat {
                    label,
                    value,
                }
            }
        }
        if !analysis.bar_energy.is_empty() {
            div {
                class: "dj-strip",
                for (label, heights) in strips {
                    div {
                        class: "dj-strip__row",
                        span {
                            class: "dj-strip__label",
                            "{label}"
                        }
                        div {
                            class: "dj-strip__bars",
                            for height in heights {
                                span {
                                    class: "dj-strip__bar",
                                    style: "height: {height:.1}%",
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The title and optional technical detail of a status that is not ready.
fn status_message(status: &AnalysisStatus) -> Option<(&'static str, Option<String>)> {
    match status {
        AnalysisStatus::Ready { .. } => None,
        AnalysisStatus::Analyzing => Some(("Analizando…", None)),
        AnalysisStatus::Deferred(DeferReason::Thermal) => Some((
            "En pausa: el equipo está caliente. Se intenta de nuevo en un minuto.",
            None,
        )),
        AnalysisStatus::Deferred(DeferReason::LowPower) => {
            Some(("En pausa por ahorro de energía o batería baja.", None))
        }
        AnalysisStatus::Unavailable(reason) => {
            Some(("No disponible en este equipo.", Some(reason.clone())))
        }
        AnalysisStatus::Failed(reason) => Some(("No se pudo analizar.", Some(reason.clone()))),
    }
}

/// Column heights in percent for a row of levels in dB (a band of every bar,
/// or the vocals every half second): one column per level, or the loudest of
/// each group when there are more levels than columns.
fn strip(levels: &[f32]) -> Vec<f32> {
    if levels.is_empty() {
        return Vec::new();
    }
    let group = levels.len().div_ceil(STRIP_COLUMNS);
    levels
        .chunks(group)
        .map(|chunk| {
            let loudest = chunk.iter().copied().fold(STRIP_FLOOR_DB, f32::max);
            ((loudest - STRIP_FLOOR_DB) / -STRIP_FLOOR_DB).clamp(0.0, 1.0) * 100.0
        })
        .collect()
}

/// `value` with `places` decimals, a decimal comma and a true minus sign.
fn decimal(value: f64, places: usize) -> String {
    format!("{value:.places$}")
        .replace('.', ",")
        .replace('-', "−")
}

fn percent(value: f32) -> String {
    format!("{} %", (value.clamp(0.0, 1.0) * 100.0).round())
}

fn tempo_label(tempo: Option<f32>) -> String {
    tempo.map_or_else(
        || "Sin pulso estable".to_owned(),
        |tempo| format!("{} BPM", decimal(tempo.into(), 1)),
    )
}

fn meter_label(meter: Option<Meter>) -> String {
    meter.map_or_else(
        || "Sin compás claro".to_owned(),
        |meter| {
            let feel = match meter.subdivision {
                Subdivision::Binary => "binario",
                Subdivision::Ternary => "ternario",
            };
            format!("{} tiempos · {feel}", meter.beats_per_bar)
        },
    )
}

fn key_label(key: Option<KeyEstimate>) -> String {
    key.map_or_else(
        || "Sin tonalidad clara".to_owned(),
        |key| {
            let tonic = usize::from(key.tonic.value());
            let (names, mode) = match key.mode {
                KeyMode::Major => (&MAJOR_TONICS, "mayor"),
                KeyMode::Minor => (&MINOR_TONICS, "menor"),
            };
            format!("{} {mode}", names[tonic % names.len()])
        },
    )
}

fn loudness_label(lufs: Option<f32>) -> String {
    lufs.map_or_else(
        || "Silencio".to_owned(),
        |lufs| format!("{} LUFS", decimal(lufs.into(), 1)),
    )
}

fn device_label(device: Option<Device>) -> &'static str {
    match device {
        Some(Device::Gpu) => "GPU",
        Some(Device::Cpu) => "CPU",
        None => "Caché",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        dj_analysis::ShownAnalysis,
        test_support::{remote_track, render},
    };
    use std::sync::Arc;
    use yt_core::{PitchClass, Track};

    fn panel() -> Element {
        rsx! {
            DjAnalysisPanel {}
        }
    }

    fn analysis() -> TrackAnalysis {
        TrackAnalysis {
            analyzer_version: yt_worker::ANALYZER_VERSION,
            sample_rate: 44_100,
            duration_frames: 44_100 * 8,
            beats: (0..16).map(|beat| beat * 20_672).collect(),
            downbeats: vec![0, 4, 8, 12],
            meter: Some(Meter {
                beats_per_bar: 4,
                subdivision: Subdivision::Binary,
                confidence: 1.0,
            }),
            tempo_bpm: Some(128.0),
            tempo_stability: 0.97,
            key: Some(KeyEstimate {
                tonic: PitchClass::new(2).unwrap(),
                mode: KeyMode::Major,
                confidence: 0.7,
            }),
            integrated_lufs: Some(-9.8),
            true_peak_dbtp: -0.9,
            short_term_lufs: vec![-10.0; 16],
            bar_energy: vec![
                BandEnergy {
                    low_db: -18.0,
                    mid_db: -22.0,
                    high_db: -30.0,
                };
                3
            ],
            sections: Vec::new(),
            vocal_db: Vec::new(),
            momentary_lufs: Vec::new(),
        }
    }

    #[test]
    fn the_panel_explains_why_nothing_is_shown_and_shows_a_ready_analysis() {
        let off = render(panel, |_| {});
        assert!(off.contains("Activa el modo DJ"), "{off}");
        let online = render(panel, |state| {
            state.set_dj_mode(true);
            state
                .current
                .set(Some(remote_track("abcdefghijk", "Online")));
        });
        assert!(online.contains("cuando termina de descargarse"), "{online}");
        let ready = render(panel, |state| {
            state.set_dj_mode(true);
            let track = Track {
                media_type: "audio".into(),
                path: "/music/song.flac".into(),
                ..remote_track("aqz-KE-bpKQ", "Song")
            };
            state.current.set(Some(track.clone()));
            state.dj_analysis.shown.set(Some(ShownAnalysis {
                track_id: track.id,
                status: AnalysisStatus::Ready {
                    analysis: Arc::new(analysis()),
                    device: Some(Device::Gpu),
                },
            }));
        });
        for expected in [
            "128,0 BPM",
            "4 tiempos · binario",
            "Re mayor",
            "−9,8 LUFS",
            "GPU",
        ] {
            assert!(ready.contains(expected), "{expected} in {ready}");
        }
        assert_eq!(
            ready.matches("dj-strip__bar\"").count(),
            9,
            "three bands of three bars"
        );
    }

    #[test]
    fn numbers_read_the_spanish_way() {
        assert_eq!(tempo_label(Some(128.04)), "128,0 BPM");
        assert_eq!(loudness_label(Some(-9.84)), "−9,8 LUFS");
        assert_eq!(tempo_label(None), "Sin pulso estable");
        assert_eq!(percent(0.974), "97 %");
    }

    #[test]
    fn keys_and_meters_are_named() {
        let key = |tonic, mode| {
            key_label(Some(KeyEstimate {
                tonic: PitchClass::new(tonic).unwrap(),
                mode,
                confidence: 0.8,
            }))
        };
        assert_eq!(key(2, KeyMode::Major), "Re mayor");
        assert_eq!(key(1, KeyMode::Minor), "Do♯ menor");
        assert_eq!(key(10, KeyMode::Major), "Si♭ mayor");
        let meter = Meter {
            beats_per_bar: 6,
            subdivision: Subdivision::Ternary,
            confidence: 0.9,
        };
        assert_eq!(meter_label(Some(meter)), "6 tiempos · ternario");
    }

    #[test]
    fn level_strips_are_bounded_and_scaled() {
        let levels: Vec<f32> = (0..300).map(|bar| -(bar % 61) as f32).collect();
        let columns = strip(&levels);
        assert!(columns.len() <= STRIP_COLUMNS);
        assert!(columns.iter().all(|height| (0.0..=100.0).contains(height)));
        assert_eq!(strip(&[0.0]), vec![100.0]);
        assert_eq!(strip(&[-90.0]), vec![0.0]);
        assert!(strip(&[]).is_empty());
    }
}
