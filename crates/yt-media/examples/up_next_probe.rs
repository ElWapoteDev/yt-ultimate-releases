//! Watch-next metadata for one video: Mix and related counts plus sample rows.
//! Reads metadata only; no stream URL is resolved or printed.
use yt_media::{CancellationToken, SearchResult, YoutubeClient, UP_NEXT_ITEM_LIMIT};

/// Luis Fonsi's official "Despacito" video, a stable and widely available seed.
const DEFAULT_VIDEO: &str = "kJQP7kiw5Fk";
const SAMPLE_ROWS: usize = 3;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_VIDEO.into());
    let up_next = YoutubeClient::new()?
        .up_next(&input, UP_NEXT_ITEM_LIMIT, CancellationToken::new())
        .await?;
    print_list("Mix", &up_next.mix);
    print_list("Related", &up_next.related);
    Ok(())
}

fn print_list(label: &str, videos: &[SearchResult]) {
    println!("{label}: {} videos", videos.len());
    for video in videos.iter().take(SAMPLE_ROWS) {
        println!(
            "  {} | {} | {} | {}:{:02}",
            video.id,
            video.title,
            video.channel,
            video.duration_seconds / 60,
            video.duration_seconds % 60
        );
    }
}
