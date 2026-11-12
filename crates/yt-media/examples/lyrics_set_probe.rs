//! Builds a lyrics alignment test set outside the repository from a list of
//! songs, one `title | artist` per line: the LRCLIB record that has both
//! synced and plain lyrics, and the YouTube audio whose duration matches the
//! record's. `lyrics_align_probe` then aligns the plain text and compares the
//! line starts with the synced ones (`docs/LYRICS_ALIGNMENT.md` §12).
//!
//! ```sh
//! cargo run --release -p yt-media --example lyrics_set_probe -- <songs.txt> <out-dir>
//! ```
//!
//! Requests go one at a time with a generic client name; only titles, artists
//! and video ids are sent. Songs already in `<out-dir>` are kept.

use std::{fs, path::Path};
use yt_core::{Lyrics, LyricsCandidate, LyricsQuery};
use yt_media::{
    CancellationToken, ConnectionOptions, Downloader, LrclibClient, SearchResult, StreamKind,
    YoutubeClient,
};

const USER_AGENT: &str = "lyrics-set-probe/1.0";
/// LRCLIB matches recordings within this many seconds, so the audio must too.
const DURATION_TOLERANCE_SECONDS: f64 = 2.0;
const VIDEO_RESULTS: usize = 15;
/// Auto-generated album uploads carry the catalog recording.
const TOPIC_SUFFIX: &str = " - Topic";
const MANIFEST_HEADER: &str =
    "slug\ttitle\tartist\tlanguage\tlrclib_seconds\tvideo\tvideo_seconds\tchannel\tword_timing\taudio";
const LANGUAGE: &str = "es";

type Res<T> = Result<T, Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [list, out] = args.as_slice() else {
        return Err("usage: <songs.txt> <out-dir>".into());
    };
    let out = Path::new(out);
    fs::create_dir_all(out.join("audio"))?;
    fs::create_dir_all(out.join("lyrics"))?;
    let lrclib = LrclibClient::new(&ConnectionOptions::default(), USER_AGENT)?;
    let youtube = YoutubeClient::new()?;
    let downloader = Downloader::new()?;
    let mut manifest = vec![MANIFEST_HEADER.to_owned()];
    for line in fs::read_to_string(list)?.lines() {
        let Some((title, artist)) = line.split_once('|') else {
            continue;
        };
        let (title, artist) = (title.trim(), artist.trim());
        match prepare(&lrclib, &youtube, &downloader, out, title, artist).await {
            Ok(row) => {
                println!("ready  {row}");
                manifest.push(row);
            }
            Err(error) => println!("skipped {title} | {artist}: {error}"),
        }
    }
    fs::write(out.join("manifest.tsv"), manifest.join("\n") + "\n")?;
    println!("{} songs in {}", manifest.len() - 1, out.display());
    Ok(())
}

async fn prepare(
    lrclib: &LrclibClient,
    youtube: &YoutubeClient,
    downloader: &Downloader,
    out: &Path,
    title: &str,
    artist: &str,
) -> Res<String> {
    let query = LyricsQuery::new(title, artist, "", 0.0)?;
    let records = lrclib.search(&query, CancellationToken::new()).await?;
    let record =
        choose_record(&records, title).ok_or("no LRCLIB record with synced and plain lyrics")?;
    let (Some(synced), Some(plain)) = (&record.synced, &record.plain) else {
        return Err("record without both lyrics".into());
    };
    let videos = youtube
        .search(&format!("{artist} {title}"), VIDEO_RESULTS)
        .await?;
    let video = choose_video(&videos, title, record.duration)
        .ok_or("no video of the record's title and duration")?;
    let slug = slug(&format!("{artist} {title}"));
    let resolved = youtube.resolve(&video.id, StreamKind::Audio).await?;
    let audio = format!("{slug}.{}.{}", video.id, resolved.extension);
    let path = out.join("audio").join(&audio);
    if !path.exists() {
        downloader
            .download_sized(
                &resolved.stream_url,
                &path,
                resolved.content_length,
                CancellationToken::new(),
                |_| {},
            )
            .await?;
    }
    fs::write(
        out.join("lyrics").join(format!("{slug}.plain.txt")),
        plain_text(plain),
    )?;
    fs::write(
        out.join("lyrics").join(format!("{slug}.synced.lrc")),
        lrc_text(synced),
    )?;
    Ok([
        slug,
        record.title.clone(),
        record.artist.clone(),
        LANGUAGE.to_owned(),
        format!("{:.1}", record.duration),
        video.id.clone(),
        video.duration_seconds.to_string(),
        video.channel.clone(),
        synced.has_word_timing().to_string(),
        audio,
    ]
    .join("\t"))
}

/// The first record (LRCLIB orders by relevance) whose title matches and that
/// has both synced and plain lyrics and a duration; one with word timing wins.
fn choose_record<'a>(records: &'a [LyricsCandidate], title: &str) -> Option<&'a LyricsCandidate> {
    let wanted = key(title);
    let usable: Vec<&LyricsCandidate> = records
        .iter()
        .filter(|record| {
            !record.instrumental
                && record.duration > 0.0
                && record.synced.is_some()
                && record.plain.is_some()
                && key(&record.title).contains(&wanted)
        })
        .collect();
    usable
        .iter()
        .find(|record| record.synced.as_ref().is_some_and(Lyrics::has_word_timing))
        .or_else(|| usable.first())
        .copied()
}

/// A video that names the song and lasts as long as the record, preferring an
/// auto-generated album upload, then the most viewed.
fn choose_video<'a>(
    videos: &'a [SearchResult],
    title: &str,
    seconds: f64,
) -> Option<&'a SearchResult> {
    let wanted = key(title);
    videos
        .iter()
        .filter(|video| {
            (video.duration_seconds as f64 - seconds).abs() <= DURATION_TOLERANCE_SECONDS
                && key(&video.title).contains(&wanted)
        })
        .max_by_key(|video| (video.channel.ends_with(TOPIC_SUFFIX), video.view_count))
}

/// Lowercase letters and digits without Spanish accents, for comparing titles.
fn key(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric())
        .map(|c| match c {
            'á' | 'à' | 'ä' => 'a',
            'é' | 'è' | 'ë' => 'e',
            'í' | 'ì' | 'ï' => 'i',
            'ó' | 'ò' | 'ö' => 'o',
            'ú' | 'ù' | 'ü' => 'u',
            'ñ' => 'n',
            other => other,
        })
        .collect()
}

/// ASCII letters, digits and single dashes, portable as a file name.
fn slug(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_owned()
}

fn plain_text(lyrics: &Lyrics) -> String {
    lyrics
        .lines()
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// Synced lyrics as LRC, with enhanced word stamps where the record has them.
fn lrc_text(lyrics: &Lyrics) -> String {
    let stamp = |ms: u32| {
        format!(
            "{:02}:{:02}.{:02}",
            ms / 60_000,
            ms / 1_000 % 60,
            ms % 1_000 / 10
        )
    };
    let mut text = String::new();
    for line in lyrics.lines() {
        text.push_str(&format!("[{}]", stamp(line.start_ms)));
        if line.words.is_empty() {
            text.push_str(&line.text);
        } else {
            for word in &line.words {
                text.push_str(&format!("<{}>{}", stamp(word.start_ms), word.text));
            }
            if let Some(end) = line.end_ms {
                text.push_str(&format!("<{}>", stamp(end)));
            }
        }
        text.push('\n');
    }
    text
}
