//! Metadata or a complete download/decode check; no signed URL is printed.
use rodio::Source;
use yt_media::{CancellationToken, Downloader, StreamKind, YoutubeClient};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let input = args
        .iter()
        .find(|v| !v.starts_with("--"))
        .map(String::as_str)
        .unwrap_or("aqz-KE-bpKQ");
    let client = YoutubeClient::new()?;
    let results = client.search("Big Buck Bunny Blender", 3).await?;
    println!("Search returned {} videos", results.len());
    let media = client.resolve(input, StreamKind::Audio).await?;
    println!(
        "Resolved {}: {} seconds, {:?} bytes",
        media.media.title, media.media.duration_seconds, media.content_length
    );
    if args.iter().any(|v| v == "--download") {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("audio.m4a");
        let receipt = Downloader::new()?
            .download_sized(
                &media.stream_url,
                &path,
                media.content_length,
                CancellationToken::new(),
                |_| {},
            )
            .await?;
        let mut decoded = yt_media::decode_file(path)?;
        let channels = decoded.channels();
        let rate = decoded.sample_rate();
        let count = decoded.by_ref().count();
        assert!(decoded.decoding_error().is_none());
        println!(
            "Complete download {} bytes; decoded {:.3} seconds",
            receipt.bytes,
            count as f64 / channels as f64 / rate as f64
        );
    }
    Ok(())
}
