#[cfg(feature = "native-stems")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{f32::consts::TAU, path::PathBuf, time::Instant};
    use yt_media::{CancellationToken, StemSeparator};
    let model = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("crates/yt-media/models/bs-roformer-v1.pt"));
    let seconds = std::env::args()
        .nth(2)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1)
        .clamp(1, 60);
    let frames = seconds * 44_100;
    let dir = tempfile::tempdir()?;
    let input = dir.path().join("input.wav");
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 44_100,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(&input, spec)?;
    for frame in 0..frames {
        writer.write_sample((frame as f32 * TAU * 440.0 / 44_100.0).sin() * 0.2)?;
        writer.write_sample((frame as f32 * TAU * 660.0 / 44_100.0).sin() * 0.2)?;
    }
    writer.finalize()?;
    let started = Instant::now();
    let separator = StemSeparator::load(model)?;
    println!("Initial device: {}", separator.device_name());
    let receipt = separator.separate(
        &input,
        dir.path().join("vocals.wav"),
        dir.path().join("instrumental.wav"),
        CancellationToken::new(),
        |p| println!("Frames: {} / {:?}", p.processed_frames, p.total_frames),
    )?;
    assert_eq!(receipt.frames, frames);
    println!(
        "Inference device: {}; fallback: {:?}",
        receipt.device, receipt.fallback_reason
    );
    let original = hound::WavReader::open(&input)?
        .into_samples::<f32>()
        .collect::<Result<Vec<_>, _>>()?;
    let vocals = hound::WavReader::open(&receipt.vocals)?
        .into_samples::<f32>()
        .collect::<Result<Vec<_>, _>>()?;
    let instrumental = hound::WavReader::open(&receipt.instrumental)?
        .into_samples::<f32>()
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(original.len(), vocals.len());
    assert_eq!(original.len(), instrumental.len());
    assert!(vocals.iter().all(|v| v.is_finite()));
    assert!(vocals.iter().any(|v| v.abs() > 1e-7));
    let maximum_error = original
        .iter()
        .zip(vocals.iter().zip(&instrumental))
        .map(|(a, (v, i))| (a - v - i).abs())
        .fold(0.0_f32, f32::max);
    assert!(maximum_error < 1e-6);
    let cancelled = CancellationToken::new();
    let cancelled_result = separator.separate(
        &input,
        dir.path().join("cancel-vocals.wav"),
        dir.path().join("cancel-instrumental.wav"),
        cancelled.clone(),
        |_| cancelled.cancel(),
    );
    assert!(matches!(
        cancelled_result,
        Err(yt_media::MediaError::Cancelled)
    ));
    assert!(!dir.path().join("cancel-vocals.wav").exists());
    assert!(!dir.path().join("cancel-instrumental.wav").exists());
    assert!(!std::fs::read_dir(dir.path())?.any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".part")));
    println!("Native TorchScript inference passed: {} stereo frames, reconstruction error {}, elapsed {:?}.", receipt.frames, maximum_error, started.elapsed());
    Ok(())
}

#[cfg(not(feature = "native-stems"))]
fn main() {
    eprintln!("This probe requires --features native-stems and LibTorch 2.12.");
}
