#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use rodio::Source;
    let id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "aqz-KE-bpKQ".into());
    let media = yt_media::YoutubeClient::new()?
        .resolve(&id, yt_media::StreamKind::Audio)
        .await?;
    let started = std::time::Instant::now();
    let (mut decoder, handle) =
        yt_media::open_http_audio(media, yt_media::CancellationToken::new()).await?;
    println!(
        "Decoder ready in {:?}; initial cache {:?}",
        started.elapsed(),
        handle.snapshot()
    );
    tokio::task::spawn_blocking(move || {
        let first: Vec<_> = decoder.by_ref().take(48000).collect();
        assert_eq!(first.len(), 48000);
        decoder
            .try_seek(std::time::Duration::from_secs(120))
            .expect("remote seek");
        assert_eq!(decoder.by_ref().take(48000).count(), 48000);
        assert!(decoder.decoding_error().is_none());
    })
    .await?;
    println!(
        "First packets and seek passed; cache {:?}",
        handle.snapshot()
    );
    handle.cancel();
    Ok(())
}
