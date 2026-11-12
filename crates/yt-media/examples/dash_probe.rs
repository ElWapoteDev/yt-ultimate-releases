use yt_media::{CancellationToken, Downloader, VideoQuality, YoutubeClient};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = YoutubeClient::new()?;
    let id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "aqz-KE-bpKQ".into());
    let details = client.video_details(&id, CancellationToken::new()).await?;
    println!(
        "{}: {} formats, {} subtitle tracks",
        details.media.title,
        details.formats.len(),
        details.subtitles.len()
    );
    for subtitle in &details.subtitles {
        println!("Subtitle {} {}", subtitle.id, subtitle.name);
    }
    let best = client
        .resolve_download(&id, VideoQuality::Best, CancellationToken::new())
        .await?;
    println!(
        "Best: {} {:?}, separate audio {}",
        best.quality_label,
        best.video.content_length,
        best.audio.is_some()
    );
    let chosen = client
        .resolve_download(&id, VideoQuality::MaxHeight(144), CancellationToken::new())
        .await?;
    println!(
        "Chosen: {} {:?}, separate audio {}",
        chosen.quality_label,
        chosen.video.content_length,
        chosen.audio.is_some()
    );
    let directory = std::env::args()
        .nth(2)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("ytu-dash-probe"));
    std::fs::create_dir_all(&directory)?;
    let downloader = Downloader::new()?;
    let video = directory.join("video.mp4");
    if !video.exists() {
        downloader
            .download_sized(
                &chosen.video.stream_url,
                &video,
                chosen.video.content_length,
                CancellationToken::new(),
                |_| {},
            )
            .await?;
    }
    if let Some(audio) = chosen.audio {
        let output = directory.join("audio.m4a");
        if !output.exists() {
            downloader
                .download_sized(
                    &audio.stream_url,
                    &output,
                    audio.content_length,
                    CancellationToken::new(),
                    |_| {},
                )
                .await?;
        }
    }
    println!(
        "Downloaded both original tracks into {}",
        directory.display()
    );
    Ok(())
}
