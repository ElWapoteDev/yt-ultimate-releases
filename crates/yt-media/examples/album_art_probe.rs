//! The album cover each given video gets, from one watch-next request per
//! video, with the URLs the application loads at each size. Reads metadata
//! only; no image or stream is downloaded.
use yt_media::{sized_artwork, CancellationToken, YoutubeClient};

/// Luis Fonsi's official "Despacito" video, a stable and widely available seed.
const DEFAULT_VIDEO: &str = "kJQP7kiw5Fk";
/// Sizes the application requests: rows, cards, and the original image.
const EDGES: [Option<u32>; 3] = [Some(120), Some(544), None];

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut videos: Vec<String> = std::env::args().skip(1).collect();
    if videos.is_empty() {
        videos.push(DEFAULT_VIDEO.into());
    }
    let youtube = YoutubeClient::new()?;
    for video in videos {
        match youtube
            .album_artwork(&video, CancellationToken::new())
            .await?
        {
            Some(cover) => {
                println!("{video}: {cover}");
                for edge in EDGES {
                    println!("  {}", sized_artwork(&cover, edge));
                }
            }
            None => println!("{video}: no album cover (keeps its thumbnail)"),
        }
    }
    Ok(())
}
