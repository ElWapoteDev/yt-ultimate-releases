use yt_media::{CancellationToken, YoutubeClient};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = YoutubeClient::new()?;
    let id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "kjBOesZCoqc".into());
    let details = client.video_details(&id, CancellationToken::new()).await?;
    println!(
        "{}: {} subtitle tracks",
        details.media.title,
        details.subtitles.len()
    );
    let track = details
        .subtitles
        .iter()
        .find(|t| t.language_code == "en")
        .or(details.subtitles.first())
        .ok_or("No subtitles")?;
    let subtitles = client
        .subtitles(&id, &track.id, CancellationToken::new())
        .await?;
    println!(
        "{}: {} cues, VTT {} bytes",
        subtitles.language,
        subtitles.segments.len(),
        subtitles.to_vtt().len()
    );
    Ok(())
}
