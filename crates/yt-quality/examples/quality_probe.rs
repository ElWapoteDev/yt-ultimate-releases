#[cfg(any(
    feature = "native-stems",
    all(feature = "metal-stems", target_os = "macos")
))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{path::PathBuf, time::Instant};
    let model = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from("crates/yt-media/models").join(yt_media::STEM_MODEL_FILENAME)
        });
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/input.wav");
    let temp = tempfile::tempdir()?;
    let cancel = yt_media::CancellationToken::new();
    let started = Instant::now();
    let prepared = temp.path().join("prepared.wav");
    yt_quality::prepare_source(source, &prepared, &cancel)?;
    let separator = yt_media::StemSeparator::load(model)?;
    let stems = separator.separate(
        prepared,
        temp.path().join("vocals.wav"),
        temp.path().join("instrumental.wav"),
        cancel.clone(),
        |_| {},
    )?;
    let reference = temp.path().join("reference.wav");
    let processing = yt_quality::process_stems(
        stems.vocals,
        stems.instrumental,
        &reference,
        "native-pipeline-probe",
        &cancel,
        |p| println!("{}: {:.0}%", p.stage, p.fraction * 100.0),
    )?;
    let encoded = temp.path().join("final.flac");
    yt_audio::convert(
        &reference,
        &encoded,
        yt_audio::AudioFormat::Flac,
        &cancel,
        |_| {},
    )?;
    let verified = yt_quality::verify_final_flac(reference, encoded, &cancel)?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"processing":processing,"verified":verified,"device":stems.device,"elapsed_seconds":started.elapsed().as_secs_f64()})
        )?
    );
    Ok(())
}
#[cfg(not(any(
    feature = "native-stems",
    all(feature = "metal-stems", target_os = "macos")
)))]
fn main() {
    eprintln!("Use --features metal-stems on macOS or --features native-stems with LibTorch 2.12.");
}
