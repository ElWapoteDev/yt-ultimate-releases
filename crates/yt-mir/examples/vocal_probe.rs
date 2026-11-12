//! Measure the vocals of one audio file with the real separation network.
//!
//! cargo run --release -p yt-mir --example vocal_probe -- \
//!     <audio file> <ONNX Runtime library> <vocal model> [--cpu] [--cache <dir>] [--reference <vocals.f32>]
//!
//! Reads only the given files. With `--reference`, the audio file is instead
//! raw little-endian `f32` planar stereo at 44.1 kHz (`models/export_htdemucs.py`
//! writes one segment and the reference implementation's vocals for it), and
//! the levels are compared with the reference's.

use rodio::Source;
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Instant};
use yt_mir::{Device, Pcm, VocalModel};

const STEP_SECONDS: f64 = 0.5;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: vocal_probe <audio> <libonnxruntime> <vocal model> [--cpu] [--cache <dir>] [--reference <vocals.f32>]";
    let audio = PathBuf::from(args.next().ok_or(usage)?);
    let library = PathBuf::from(args.next().ok_or(usage)?);
    let model_path = PathBuf::from(args.next().ok_or(usage)?);
    let (mut device, mut cache, mut reference) = (Device::Gpu, None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cpu" => device = Device::Cpu,
            "--cache" => cache = Some(PathBuf::from(args.next().ok_or(usage)?)),
            "--reference" => reference = Some(PathBuf::from(args.next().ok_or(usage)?)),
            _ => return Err(usage.into()),
        }
    }
    yt_mir::init_runtime(&library)?;
    let began = Instant::now();
    let mut model = VocalModel::load(&model_path, device, cache.as_deref())?;
    println!(
        "device {device:?}; model load {:.1} s",
        began.elapsed().as_secs_f64()
    );
    let (samples, channels, rate) = match &reference {
        Some(_) => {
            let planar = read_f32(&audio)?;
            let half = planar.len() / 2;
            let interleaved = (0..half)
                .flat_map(|n| [planar[n], planar[half + n]])
                .collect::<Vec<f32>>();
            (interleaved, 2, 44_100)
        }
        None => {
            let source = yt_media::decode_file(&audio)?;
            let (channels, rate) = (source.channels(), source.sample_rate());
            (source.collect(), channels, rate)
        }
    };
    let pcm = Pcm::new(&samples, channels, rate)?;
    let started = Instant::now();
    let levels = model.measure(&pcm, &AtomicBool::new(false))?;
    let seconds = pcm.frames() as f64 / f64::from(rate);
    println!(
        "{seconds:.1} s of audio measured in {:.2} s ({} steps)",
        started.elapsed().as_secs_f64(),
        levels.len()
    );
    if let Some(reference) = reference {
        let planar = read_f32(&reference)?;
        let half = planar.len() / 2;
        let step = (STEP_SECONDS * 44_100.0) as usize;
        let expected: Vec<f32> = (0..half.div_ceil(step))
            .map(|index| {
                let range = index * step..((index + 1) * step).min(half);
                let count = range.len().max(1) as f64;
                let power: f64 = range
                    .map(|n| {
                        f64::from(planar[n] * planar[n] + planar[half + n] * planar[half + n]) / 2.0
                    })
                    .sum();
                (10.0 * (power / count).max(1e-30).log10()) as f32
            })
            .collect();
        let worst = levels
            .iter()
            .zip(&expected)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        for (index, (ours, theirs)) in levels.iter().zip(&expected).enumerate() {
            println!("step {index:3}: {ours:8.2} dB, reference {theirs:8.2} dB");
        }
        println!(
            "largest difference from the reference: {worst:.3} dB over {} steps",
            expected.len()
        );
        // A window separated on its own, with its context, against the same
        // samples of the reference.
        let window = 44_100 * 5..(44_100 * 15).min(half as u64);
        if window.start < window.end {
            let started = Instant::now();
            let alone = model.separate(&pcm, window.clone(), &AtomicBool::new(false))?;
            let (mut error, mut power) = (0.0_f64, 0.0_f64);
            for (index, sample) in alone.iter().enumerate() {
                let (frame, channel) = (index / 2, index % 2);
                let reference = planar[channel * half + window.start as usize + frame];
                error += f64::from(sample - reference).powi(2);
                power += f64::from(reference).powi(2);
            }
            println!(
                "window 5–15 s separated alone in {:.2} s: error {:.1} dB below the vocals",
                started.elapsed().as_secs_f64(),
                10.0 * (power / error.max(1e-30)).log10()
            );
        }
    } else {
        for (index, chunk) in levels.chunks(4).enumerate() {
            let loudest = chunk.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let bar = "#".repeat(((loudest + 60.0).max(0.0) / 2.0) as usize);
            println!(
                "{:6.1} s {loudest:7.1} dB {bar}",
                index as f64 * 4.0 * STEP_SECONDS
            );
        }
    }
    Ok(())
}

fn read_f32(path: &PathBuf) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| f32::from_le_bytes(*word))
        .collect())
}
