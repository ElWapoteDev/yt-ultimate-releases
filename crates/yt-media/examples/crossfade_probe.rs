//! Blend two local files on the real output device and print the engine state.
//! Usage: `cargo run -p yt-media --example crossfade_probe -- <first> <second> [--audible]`.
//! Muted unless `--audible` is given.
use std::time::Duration;
use yt_media::{AudioPlayer, Crossfade, FadeCurve, Transition};

const FADE: Duration = Duration::from_secs(3);
const SAMPLE: Duration = Duration::from_millis(250);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let files: Vec<&String> = args.iter().filter(|arg| !arg.starts_with("--")).collect();
    let [first, second] = files.as_slice() else {
        return Err("expected two audio files".into());
    };
    let player = AudioPlayer::new()?;
    player.set_volume(if args.iter().any(|arg| arg == "--audible") {
        0.3
    } else {
        0.0
    })?;
    player.play(first)?;
    std::thread::sleep(Duration::from_secs(2));
    let before = player.snapshot();
    println!(
        "before: {:?} at {:.2}s",
        before.path, before.position_seconds
    );
    player.play_with(
        second,
        Transition::Crossfade(Crossfade::new(FADE, FadeCurve::EqualPower)?),
    )?;
    for _ in 0..((FADE + Duration::from_secs(2)).as_millis() / SAMPLE.as_millis()) {
        let snapshot = player.snapshot();
        println!(
            "{:>5.2}s playing={} crossfading={} path={:?}",
            snapshot.position_seconds,
            snapshot.is_playing,
            snapshot.crossfading,
            snapshot.path.as_ref().and_then(|path| path.file_name())
        );
        std::thread::sleep(SAMPLE);
    }
    player.seek(10.0)?;
    std::thread::sleep(SAMPLE * 2);
    let after_seek = player.snapshot();
    println!(
        "after seek: {:.2}s crossfading={}",
        after_seek.position_seconds, after_seek.crossfading
    );
    player.stop()?;
    Ok(())
}
