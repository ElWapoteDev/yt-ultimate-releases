//! Public search/temporary playback integration probe; no signed URL is logged.
use yt_media::{CancellationToken, VideoQuality, YoutubeClient};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let query = args.first().map(String::as_str).unwrap_or("peso pluma");
    let youtube = YoutubeClient::new()?;
    let results = youtube.search(query, 25).await?;
    let mut ids = std::collections::HashSet::new();
    for item in &results {
        assert!(ids.insert(&item.id), "Search duplicated an identity");
        println!("{} {:>4}s {}", item.id, item.duration_seconds, item.title);
    }
    if args.iter().any(|v| v == "--prepare") {
        let item = results
            .iter()
            .find(|item| item.title.to_lowercase().contains("hollywood"))
            .or_else(|| results.first())
            .ok_or("No results")?;
        let started = std::time::Instant::now();
        let video = youtube
            .prepare_video(
                &item.url,
                VideoQuality::MaxHeight(360),
                CancellationToken::new(),
                |_| {},
            )
            .await?;
        let mut decoded = yt_media::decode_file(video.path())?;
        use rodio::Source;
        let rate = decoded.sample_rate();
        let channels = decoded.channels();
        let samples = decoded.by_ref().count();
        assert!(decoded.decoding_error().is_none());
        println!(
            "Prepared {}p, {} bytes in {:.2}s; decoded {:.3}s audio",
            video.height,
            std::fs::metadata(video.path())?.len(),
            started.elapsed().as_secs_f64(),
            samples as f64 / rate as f64 / channels as f64
        );
    }
    Ok(())
}
