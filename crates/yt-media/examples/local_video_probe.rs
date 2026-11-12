use std::{fs::OpenOptions, path::PathBuf};
use yt_media::{prepare_local_video, CancellationToken};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let input = PathBuf::from(
        args.next()
            .ok_or("Usage: local_video_probe input.mkv [new-output.mp4]")?,
    );
    let prepared = prepare_local_video(&input, CancellationToken::new(), |_| {})?;
    println!(
        "Prepared {}×{}, {:.6}s, {} bytes",
        prepared.width(),
        prepared.height(),
        prepared.duration_seconds(),
        std::fs::metadata(prepared.path())?.len()
    );
    if let Some(destination) = args.next() {
        let mut input = std::fs::File::open(prepared.path())?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        println!("Saved {destination}");
    }
    Ok(())
}
