//! Measure how one audio file sounds and what the measurement costs.
//! Usage: `cargo run --release -p yt-media --example analysis_probe -- <file> [start-s] [length-s]`.
//! Prints the features from `analyze_file` (decoding included), then times
//! `Analyzer` alone on the same amount of audio, decoded in advance from the
//! beginning of the file.
use rodio::Source;
use std::{
    hint::black_box,
    time::{Duration, Instant},
};
use yt_media::{analyze_file, decode_file, Analyzer, CancellationToken};

fn seconds(argument: Option<String>, default: Duration) -> Result<Duration, String> {
    match argument {
        None => Ok(default),
        Some(text) => text
            .parse::<f64>()
            .ok()
            .and_then(|value| Duration::try_from_secs_f64(value).ok())
            .ok_or(format!("invalid number of seconds: {text}")),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("expected an audio file")?;
    let start = seconds(args.next(), Duration::ZERO)?;
    let length = seconds(args.next(), Duration::MAX)?;

    let begun = Instant::now();
    let features = analyze_file(path.as_ref(), start, length, &CancellationToken::new())?;
    let elapsed = begun.elapsed();
    println!("{features:#?}");
    println!(
        "analyze_file: {:.1} ms for {:.1} s of audio, decoding included",
        elapsed.as_secs_f64() * 1e3,
        features.analyzed_seconds
    );

    let mut source = decode_file(&path)?;
    let (rate, channels) = (source.sample_rate(), source.channels());
    let samples: Vec<f32> = source
        .by_ref()
        .take((features.analyzed_seconds * f64::from(rate) * f64::from(channels)) as usize)
        .collect();
    let mut analyzer = Analyzer::new(rate, channels);
    let begun = Instant::now();
    for sample in samples {
        analyzer.push(sample);
    }
    let pushed = begun.elapsed();
    let begun = Instant::now();
    black_box(analyzer.features());
    let computed = begun.elapsed();
    println!(
        "Analyzer::push: {:.0} µs per second of {rate} Hz × {channels} audio ({:.3} % of one core); features(): {:.1} µs",
        pushed.as_secs_f64() * 1e6 / analyzer.analyzed_seconds(),
        pushed.as_secs_f64() * 100.0 / analyzer.analyzed_seconds(),
        computed.as_secs_f64() * 1e6
    );
    Ok(())
}
