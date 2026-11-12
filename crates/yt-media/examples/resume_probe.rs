use rodio::Source;
use yt_media::{CancellationToken, Downloader, MediaError, ResumeKey, StreamKind, YoutubeClient};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = YoutubeClient::new()?;
    let downloader = Downloader::new()?;
    let media = client.resolve("aqz-KE-bpKQ", StreamKind::Audio).await?;
    let key = ResumeKey::for_media(&media)?;
    let dir = tempfile::tempdir()?;
    let output = dir.path().join("audio.m4a");
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let first = downloader
        .download_resumable(&media.stream_url, &output, key, cancel, move |p| {
            if p.downloaded_bytes >= 1024 * 1024 {
                trigger.cancel()
            }
        })
        .await;
    assert!(matches!(first, Err(MediaError::Cancelled)));
    let status = downloader
        .resume_status(&output)?
        .ok_or("Missing checkpoint")?;
    println!("Paused at {} bytes", status.completed_bytes);
    let refreshed = client.resolve("aqz-KE-bpKQ", StreamKind::Audio).await?;
    let result = Downloader::new()?
        .download_resumable(
            &refreshed.stream_url,
            &output,
            ResumeKey::for_media(&refreshed)?,
            CancellationToken::new(),
            |_| {},
        )
        .await?;
    let mut decoder = yt_media::decode_file(&output)?;
    let denominator = decoder.channels() as f64 * decoder.sample_rate() as f64;
    let count = decoder.by_ref().count();
    assert!(decoder.decoding_error().is_none());
    println!(
        "Resumed complete file: {} bytes; decoded {:.3} seconds",
        result.bytes,
        count as f64 / denominator
    );
    Ok(())
}
