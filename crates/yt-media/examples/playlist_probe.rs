#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "PL49CF3715CB9EF31D".into());
    let limit = std::env::args()
        .nth(2)
        .and_then(|value| value.parse().ok())
        .unwrap_or(500);
    let playlist = yt_media::YoutubeClient::new()?
        .playlist_with_limit(&input, limit, yt_media::CancellationToken::new())
        .await?;
    println!(
        "Title: {}\nChannel: {}\nItems: {}\nReported total: {:?}\nUnavailable: {}\nTruncated: {}",
        playlist.title,
        playlist.channel,
        playlist.items.len(),
        playlist.reported_total,
        playlist.unavailable_items,
        playlist.truncated
    );
    for item in playlist.items.iter().take(3) {
        println!("{} — {}", item.id, item.title);
    }
    Ok(())
}
