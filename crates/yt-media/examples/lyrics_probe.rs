//! Live LRCLIB check for one song: the exact lookup and the search records.
//! `cargo run -p yt-media --example lyrics_probe -- "<title>" "<artist>" [seconds]`
//! Sends only the title, artist and duration, with a generic client name.
use std::time::Instant;
use yt_core::{Lyrics, LyricsCandidate, LyricsQuery};
use yt_media::{CancellationToken, ConnectionOptions, LrclibClient};

const DEFAULT_TITLE: &str = "Bailando";
const DEFAULT_ARTIST: &str = "Enrique Iglesias";
const DEFAULT_SECONDS: f64 = 244.0;
const SAMPLE_ROWS: usize = 5;
const PROBE_USER_AGENT: &str = "lyrics-probe/1.0";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let title = args.next().unwrap_or_else(|| DEFAULT_TITLE.into());
    let artist = args.next().unwrap_or_else(|| DEFAULT_ARTIST.into());
    let seconds = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_SECONDS);
    let query = LyricsQuery::new(&title, &artist, "", seconds)?;
    let client = LrclibClient::new(&ConnectionOptions::default(), PROBE_USER_AGENT)?;
    let started = Instant::now();
    let exact = client.get(&query, CancellationToken::new()).await?;
    println!("exact lookup in {:?}:", started.elapsed());
    match &exact {
        Some(candidate) => print_candidate(candidate),
        None => println!("  none"),
    }
    let started = Instant::now();
    let records = client.search(&query, CancellationToken::new()).await?;
    let word_timed = records
        .iter()
        .filter(|candidate| {
            candidate
                .synced
                .as_ref()
                .is_some_and(Lyrics::has_word_timing)
        })
        .count();
    println!(
        "search: {} records, {word_timed} with word timing, in {:?}",
        records.len(),
        started.elapsed()
    );
    for candidate in records.iter().take(SAMPLE_ROWS) {
        print_candidate(candidate);
    }
    Ok(())
}

fn print_candidate(candidate: &LyricsCandidate) {
    let lines = |lyrics: &Option<Lyrics>| lyrics.as_ref().map_or(0, |lyrics| lyrics.lines().len());
    let word_lines = candidate.synced.as_ref().map_or(0, |lyrics| {
        lyrics
            .lines()
            .iter()
            .filter(|line| !line.words.is_empty())
            .count()
    });
    println!(
        "  {} | {} | {:.1} s | synced {} lines, {word_lines} with word timing | plain {} lines | instrumental {}",
        candidate.title,
        candidate.artist,
        candidate.duration,
        lines(&candidate.synced),
        lines(&candidate.plain),
        candidate.instrumental
    );
}
