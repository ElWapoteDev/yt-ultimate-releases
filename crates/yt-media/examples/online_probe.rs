//! Explicit network diagnostic; optional --download uses a temporary directory.
use yt_media::{CancellationToken, Downloader, StreamKind, YoutubeClient};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let query = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Big Buck Bunny Blender".into());
    let client = YoutubeClient::new()?;
    let results = client.search(&query, 3).await?;
    for result in &results {
        println!(
            "{} | {} | {} s",
            result.id, result.title, result.duration_seconds
        );
    }
    if let Some(first) = results.first() {
        match client.resolve(&first.url, StreamKind::Audio).await {
            Ok(resolved) => {
                println!(
                    "Resolved {} ({}, {:?} bytes)",
                    resolved.media.title, resolved.mime_type, resolved.content_length
                );
                if std::env::args().any(|argument| argument == "--download") {
                    let directory = tempfile::tempdir()?;
                    let path = directory.path().join("probe.m4a");
                    let receipt = Downloader::new()?
                        .download_sized(
                            &resolved.stream_url,
                            &path,
                            resolved.content_length,
                            CancellationToken::new(),
                            |_| {},
                        )
                        .await?;
                    if let Some(expected) = resolved.content_length {
                        assert_eq!(receipt.bytes, expected);
                    }
                    let source = yt_media::decode_file(path)?;
                    assert!(source.take(48_000).count() > 0);
                    println!(
                        "Downloaded and decoded {} bytes using Rust; temporary file removed.",
                        receipt.bytes
                    );
                }
            }
            Err(error) => {
                eprintln!("Resolution failed: {error}");
                std::process::exit(2);
            }
        }
    }
    Ok(())
}
