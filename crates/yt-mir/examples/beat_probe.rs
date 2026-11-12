//! Analyze one audio file with the real beat model and print what was found.
//!
//! cargo run --release -p yt-mir --example beat_probe -- \
//!     <audio file> <ONNX Runtime library> <beat model> [--cpu] [--cache <dir>] [--beats-json <file>]
//!
//! Reads only the given files. `--cache` keeps CoreML's compiled model so the
//! next run loads it without compiling; `--beats-json` writes beat and
//! downbeat times in seconds for comparing with the Python reference.

use rodio::Source;
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Instant};
use yt_mir::{Analyzer, BeatModel, Device, Pcm};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let usage =
        "usage: beat_probe <audio> <libonnxruntime> <beat model> [--cpu] [--cache <dir>] [--beats-json <file>]";
    let audio = PathBuf::from(args.next().ok_or(usage)?);
    let library = PathBuf::from(args.next().ok_or(usage)?);
    let model_path = PathBuf::from(args.next().ok_or(usage)?);
    let (mut device, mut cache, mut beats_json) = (Device::Gpu, None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cpu" => device = Device::Cpu,
            "--cache" => cache = Some(PathBuf::from(args.next().ok_or(usage)?)),
            "--beats-json" => beats_json = Some(PathBuf::from(args.next().ok_or(usage)?)),
            _ => return Err(usage.into()),
        }
    }
    yt_mir::init_runtime(&library)?;
    let began = Instant::now();
    let model = BeatModel::load(&model_path, device, cache.as_deref())?;
    let loaded = began.elapsed();
    let source = yt_media::decode_file(&audio)?;
    let (channels, rate) = (source.channels(), source.sample_rate());
    let samples: Vec<f32> = source.collect();
    let pcm = Pcm::new(&samples, channels, rate)?;
    let mut analyzer = Analyzer::new(model);
    let started = Instant::now();
    let analysis = analyzer.analyze(&pcm, &AtomicBool::new(false))?;
    let analyzed = started.elapsed();
    let seconds = |frame: u64| analysis.seconds(frame);
    println!(
        "device {device:?}; model load {:.2} s; analysis {:.3} s",
        loaded.as_secs_f64(),
        analyzed.as_secs_f64()
    );
    println!(
        "duration {:.1} s; {} beats; {} downbeats; tempo {:?} BPM; stability {:.2}",
        seconds(analysis.duration_frames),
        analysis.beats.len(),
        analysis.downbeats.len(),
        analysis.tempo_bpm,
        analysis.tempo_stability
    );
    println!("meter {:?}; key {:?}", analysis.meter, analysis.key);
    println!(
        "sound from {:.1} s; music until {:.1} s of {:.1} s",
        seconds(analysis.audible_start()),
        seconds(analysis.audible_end()),
        seconds(analysis.duration_frames)
    );
    let sections: Vec<String> = analysis
        .sections
        .iter()
        .map(|section| {
            let at = analysis
                .downbeats
                .get(section.start_bar as usize)
                .and_then(|index| analysis.beats.get(*index as usize))
                .map_or(0.0, |frame| seconds(*frame));
            format!(
                "bar {} ({at:.1} s) {} {:.1} dB",
                section.start_bar,
                if section.high_energy {
                    "strong"
                } else {
                    "calm"
                },
                section.level_db
            )
        })
        .collect();
    println!("sections: {}", sections.join(" | "));
    println!(
        "integrated {:?} LUFS; true peak {:.2} dBTP; {} bars of band energy",
        analysis.integrated_lufs,
        analysis.true_peak_dbtp,
        analysis.bar_energy.len()
    );
    if let Some(path) = beats_json {
        let beats: Vec<f64> = analysis.beats.iter().map(|frame| seconds(*frame)).collect();
        let downbeats: Vec<f64> = analysis
            .downbeats
            .iter()
            .map(|index| beats[*index as usize])
            .collect();
        std::fs::write(
            path,
            format!("{{\"beats\":{beats:?},\"downbeats\":{downbeats:?}}}"),
        )?;
    }
    Ok(())
}
