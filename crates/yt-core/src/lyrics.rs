//! Song lyrics: validated synced or plain lines, the parsers for the formats
//! lyrics arrive in (LRC with optional enhanced word stamps, Lyricsfile
//! documents, ID3v2 SYLT entries, plain text), the lyrics stored with library
//! files, and the cached outcome of catalog lookups.

mod lyricsfile;

use crate::{
    AppCore, CoreError, Result,
    media::{MAX_ARTIST_CHARS, MAX_DURATION_SECONDS, MAX_TITLE_CHARS, bounded_text},
};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

/// Most lines kept from one song.
pub const MAX_LYRICS_LINES: usize = 1_000;
/// Longest kept line, in characters; longer lines are cut.
pub const MAX_LINE_CHARS: usize = 300;
/// Most timed words kept per line; a line with more keeps only line timing.
pub const MAX_LINE_WORDS: usize = 80;
/// Largest lyrics text accepted for parsing, in bytes.
pub const MAX_LYRICS_BYTES: usize = 256 * 1024;
/// Latest accepted timestamp: the longest accepted media duration.
const MAX_TIMESTAMP_MS: u32 = (MAX_DURATION_SECONDS as u32) * 1_000;
/// Largest `[offset:]` honoured, in milliseconds.
const MAX_LRC_OFFSET_MS: i64 = 3_600_000;
/// Longest LRC metadata key, such as `ar`, `length` or `offset`.
const MAX_METADATA_KEY_CHARS: usize = 16;
/// Cache namespace of catalog lookups, followed by the media id.
const CACHE_PREFIX: &str = "lyrics:";
/// Longest accepted media id: a library track id or `youtube:<id>`.
const MAX_MEDIA_ID_BYTES: usize = 128;
/// File extension of sidecar lyrics next to a media file.
const SIDECAR_EXTENSION: &str = "lrc";
/// Vorbis comment that Mp3tag and Bandcamp write lyrics to, besides `LYRICS`.
const UNSYNCED_LYRICS_KEY: &str = "UNSYNCEDLYRICS";

/// One timed word or syllable of a line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricWord {
    /// Milliseconds from the start of the media.
    pub start_ms: u32,
    /// The word as written, including the space that follows it.
    pub text: String,
}

/// One line of lyrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricLine {
    /// Milliseconds from the start of the media; 0 in plain lyrics.
    pub start_ms: u32,
    /// The line as shown. In synced lyrics an empty line marks a break.
    pub text: String,
    /// Word timing when the source has it: ascending from `start_ms`, and
    /// joined together the words spell `text`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<LyricWord>,
    /// When the last word ends, when the source marks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<u32>,
}

/// Validated lyrics of one song: 1–`MAX_LYRICS_LINES` lines of at most
/// `MAX_LINE_CHARS` characters without control characters, at least one of
/// them with text. Synced lines are in time order; plain lines are untimed.
/// Deserializing validates again, so a stored value is never trusted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "LyricsRecord")]
pub struct Lyrics {
    synced: bool,
    lines: Vec<LyricLine>,
}

#[derive(Deserialize)]
struct LyricsRecord {
    synced: bool,
    lines: Vec<LyricLine>,
}

impl TryFrom<LyricsRecord> for Lyrics {
    type Error = CoreError;

    fn try_from(record: LyricsRecord) -> Result<Self> {
        let lyrics = Self {
            synced: record.synced,
            lines: record.lines,
        };
        lyrics.validate()?;
        Ok(lyrics)
    }
}

impl Lyrics {
    /// Parse LRC (time tags, several tags per line, `[offset:]`, enhanced
    /// `<mm:ss.xx>` word stamps) or, when no line is timed, plain text. Text
    /// is cleaned and cut to the bounds; `None` when nothing usable remains
    /// or `text` exceeds `MAX_LYRICS_BYTES`.
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() > MAX_LYRICS_BYTES {
            return None;
        }
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut offset_ms = 0i64;
        let mut timed = Vec::new();
        let mut plain = Vec::new();
        for raw in text.lines() {
            match lrc_line(raw) {
                LrcLine::Timed { starts, body } => {
                    let single = match starts.as_slice() {
                        [start] => Some(*start),
                        _ => None,
                    };
                    let (text, words, end_ms) = timed_body(body, single);
                    for start_ms in starts {
                        timed.push(LyricLine {
                            start_ms,
                            text: text.clone(),
                            words: words.clone(),
                            end_ms,
                        });
                    }
                }
                LrcLine::Offset(value) => offset_ms = value,
                LrcLine::Metadata => {}
                LrcLine::Text(line) => plain.push(clean_text(line)),
            }
        }
        if timed.iter().any(|line| !line.text.is_empty()) {
            Self::timed(timed, offset_ms)
        } else {
            Self::untimed(plain)
        }
    }

    /// Lyrics from timed text entries, such as an ID3v2 SYLT frame in
    /// milliseconds. An entry starting with a line break begins a new line;
    /// without any break, every entry is a line of its own.
    pub fn from_timed_entries(entries: &[(u32, String)]) -> Option<Self> {
        let grouped = entries
            .iter()
            .any(|(_, text)| text.starts_with(['\n', '\r']));
        let mut lines: Vec<(u32, Vec<LyricWord>)> = Vec::new();
        for (start_ms, text) in entries {
            let breaks = text.starts_with(['\n', '\r']);
            let word = LyricWord {
                start_ms: *start_ms,
                text: text.trim_start_matches(['\n', '\r']).to_owned(),
            };
            match lines.last_mut() {
                Some((_, words)) if grouped && !breaks => words.push(word),
                _ => lines.push((*start_ms, vec![word])),
            }
        }
        let lines = lines
            .into_iter()
            .map(|(start_ms, words)| {
                let (text, words) = normalize_words(words);
                LyricLine {
                    start_ms,
                    text,
                    words,
                    end_ms: None,
                }
            })
            .collect();
        Self::timed(lines, 0)
    }

    /// Whether lines carry start times.
    pub fn is_synced(&self) -> bool {
        self.synced
    }

    /// Whether at least one line carries word timing.
    pub fn has_word_timing(&self) -> bool {
        self.lines.iter().any(|line| !line.words.is_empty())
    }

    /// The lines, in time order when synced.
    pub fn lines(&self) -> &[LyricLine] {
        &self.lines
    }

    fn timed(mut lines: Vec<LyricLine>, offset_ms: i64) -> Option<Self> {
        let shift =
            |ms: u32| (i64::from(ms) - offset_ms).clamp(0, i64::from(MAX_TIMESTAMP_MS)) as u32;
        for line in &mut lines {
            line.start_ms = shift(line.start_ms);
            line.end_ms = line.end_ms.map(shift);
            for word in &mut line.words {
                word.start_ms = shift(word.start_ms);
            }
        }
        lines.sort_by_key(|line| line.start_ms);
        let mut kept: Vec<LyricLine> = Vec::with_capacity(lines.len());
        for line in lines {
            let repeated_break =
                line.text.is_empty() && kept.last().is_none_or(|previous| previous.text.is_empty());
            if !repeated_break {
                kept.push(line);
            }
        }
        kept.truncate(MAX_LYRICS_LINES);
        let lyrics = Self {
            synced: true,
            lines: kept,
        };
        lyrics.validate().ok().map(|()| lyrics)
    }

    fn untimed(lines: Vec<String>) -> Option<Self> {
        let mut kept: Vec<LyricLine> = Vec::with_capacity(lines.len());
        for text in lines {
            let repeated_break =
                text.is_empty() && kept.last().is_none_or(|previous| previous.text.is_empty());
            if !repeated_break {
                kept.push(LyricLine {
                    start_ms: 0,
                    text,
                    words: Vec::new(),
                    end_ms: None,
                });
            }
        }
        while kept.last().is_some_and(|line| line.text.is_empty()) {
            kept.pop();
        }
        kept.truncate(MAX_LYRICS_LINES);
        let lyrics = Self {
            synced: false,
            lines: kept,
        };
        lyrics.validate().ok().map(|()| lyrics)
    }

    fn validate(&self) -> Result<()> {
        let invalid = |reason: &str| Err(CoreError::Invalid(format!("Invalid lyrics: {reason}")));
        if self.lines.is_empty() || self.lines.len() > MAX_LYRICS_LINES {
            return invalid("line count is outside its bounds");
        }
        if self.lines.iter().all(|line| line.text.is_empty()) {
            return invalid("no line has text");
        }
        let mut previous_start = 0;
        for line in &self.lines {
            if line.text.trim() != line.text || !bounded_text(&line.text, MAX_LINE_CHARS) {
                return invalid("a line is not clean text within bounds");
            }
            if !self.synced {
                if line.start_ms != 0 || !line.words.is_empty() || line.end_ms.is_some() {
                    return invalid("plain lines cannot be timed");
                }
                continue;
            }
            if line.start_ms < previous_start || line.start_ms > MAX_TIMESTAMP_MS {
                return invalid("lines are not in time order");
            }
            previous_start = line.start_ms;
            if line.words.is_empty() {
                if line.end_ms.is_some() {
                    return invalid("a line end needs word timing");
                }
                continue;
            }
            let mut word_start = line.start_ms;
            for word in &line.words {
                if word.start_ms < word_start
                    || word.start_ms > MAX_TIMESTAMP_MS
                    || word.text.is_empty()
                    || word.text.chars().any(char::is_control)
                {
                    return invalid("words are not clean and in time order");
                }
                word_start = word.start_ms;
            }
            if line.words.len() > MAX_LINE_WORDS
                || line
                    .end_ms
                    .is_some_and(|end| end < word_start || end > MAX_TIMESTAMP_MS)
                || line
                    .words
                    .iter()
                    .map(|word| word.text.as_str())
                    .collect::<String>()
                    != line.text
            {
                return invalid("words do not spell their line");
            }
        }
        Ok(())
    }
}

/// What a lyrics catalog is asked for: the song as its metadata names it.
#[derive(Debug, Clone, PartialEq)]
pub struct LyricsQuery {
    title: String,
    artist: String,
    album: String,
    duration: f64,
}

impl LyricsQuery {
    /// A query with trimmed text. The title is required; text is bounded like
    /// remote media and free of control characters; `duration` is seconds
    /// within the media bounds, 0 when unknown. Anything else is rejected.
    pub fn new(title: &str, artist: &str, album: &str, duration: f64) -> Result<Self> {
        let query = Self {
            title: title.trim().to_owned(),
            artist: artist.trim().to_owned(),
            album: album.trim().to_owned(),
            duration,
        };
        if query.title.is_empty()
            || !bounded_text(&query.title, MAX_TITLE_CHARS)
            || !bounded_text(&query.artist, MAX_ARTIST_CHARS)
            || !bounded_text(&query.album, MAX_TITLE_CHARS)
            || !duration.is_finite()
            || !(0.0..=MAX_DURATION_SECONDS).contains(&duration)
        {
            return Err(CoreError::Invalid("Invalid lyrics query".into()));
        }
        Ok(query)
    }

    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn artist(&self) -> &str {
        &self.artist
    }
    pub fn album(&self) -> &str {
        &self.album
    }
    /// Seconds; 0 when unknown.
    pub fn duration(&self) -> f64 {
        self.duration
    }
}

/// One record a lyrics catalog offers for a query, already validated.
#[derive(Debug, Clone, PartialEq)]
pub struct LyricsCandidate {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Seconds; 0 when unknown.
    pub duration: f64,
    /// The catalog marks the recording as having no vocals.
    pub instrumental: bool,
    pub synced: Option<Lyrics>,
    pub plain: Option<Lyrics>,
}

impl LyricsCandidate {
    /// Validate a catalog record whose lyrics texts the caller parsed with
    /// the parser of each text's format. Titles and names must be bounded
    /// text, an unusable duration reads as unknown, and `synced` lyrics that
    /// have no timed line are dropped. `None` when the record names no song
    /// or offers nothing to show.
    pub fn from_record(
        title: &str,
        artist: &str,
        album: &str,
        duration: f64,
        instrumental: bool,
        synced: Option<Lyrics>,
        plain: Option<Lyrics>,
    ) -> Option<Self> {
        let query = LyricsQuery::new(title, artist, album, 0.0).ok()?;
        let synced = synced.filter(Lyrics::is_synced);
        if !instrumental && synced.is_none() && plain.is_none() {
            return None;
        }
        Some(Self {
            title: query.title,
            artist: query.artist,
            album: query.album,
            duration: if duration.is_finite() && (0.0..=MAX_DURATION_SECONDS).contains(&duration) {
                duration
            } else {
                0.0
            },
            instrumental,
            synced,
            plain,
        })
    }
}

/// Where shown lyrics come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LyricsSource {
    /// A `.lrc` file with the media file's name, next to it.
    Sidecar,
    /// Tags inside the media file.
    Embedded,
    /// The LRCLIB online catalog.
    Lrclib,
}

/// The outcome of looking a song up in a lyrics catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LyricsLookup {
    Found {
        lyrics: Lyrics,
        source: LyricsSource,
    },
    /// The catalog knows the recording has no vocals.
    Instrumental,
    /// The catalog has nothing for the song.
    Missing,
}

/// A cached lookup and when it was made (Unix seconds).
#[derive(Debug, Clone, PartialEq)]
pub struct CachedLyrics {
    pub lookup: LyricsLookup,
    pub fetched_at: f64,
}

impl AppCore {
    /// Lyrics stored with a library track, best first: synced lyrics from a
    /// sidecar `.lrc` file, then from embedded tags (LRC text or an ID3v2
    /// SYLT frame in milliseconds), then plain lyrics from either. The sidecar
    /// must resolve to a regular file in the media file's own folder and stay
    /// within `MAX_LYRICS_BYTES`. `Ok(None)` when the track has none or its
    /// file is missing; an unknown id is an error.
    pub fn local_lyrics(&self, track_id: &str) -> Result<Option<(Lyrics, LyricsSource)>> {
        let track = self.track(track_id)?;
        if track.missing {
            return Ok(None);
        }
        let sidecar = sidecar_lyrics(&track.path).map(|lyrics| (lyrics, LyricsSource::Sidecar));
        if sidecar
            .as_ref()
            .is_some_and(|(lyrics, _)| lyrics.is_synced())
        {
            return Ok(sidecar);
        }
        let embedded = embedded_lyrics(&track.path).map(|lyrics| (lyrics, LyricsSource::Embedded));
        if embedded
            .as_ref()
            .is_some_and(|(lyrics, _)| lyrics.is_synced())
        {
            return Ok(embedded);
        }
        Ok(sidecar.or(embedded))
    }

    /// The cached catalog lookup for `media_id`, if any. Stored lyrics are
    /// validated again; an unreadable entry reads as absent.
    pub fn cached_lyrics(&self, media_id: &str) -> Result<Option<CachedLyrics>> {
        let Some(payload) = self.cached_payload(&cache_key(media_id)?)? else {
            return Ok(None);
        };
        Ok(serde_json::from_str::<LyricsLookup>(&payload.json)
            .ok()
            .map(|lookup| CachedLyrics {
                lookup,
                fetched_at: payload.fetched_at,
            }))
    }

    /// Remember the catalog lookup for `media_id`, replacing any previous
    /// one. The entry lives in the bounded provider cache, so it is pruned
    /// with it and erased with the listening history.
    pub fn store_lyrics(&self, media_id: &str, lookup: &LyricsLookup) -> Result<()> {
        if let LyricsLookup::Found { lyrics, .. } = lookup {
            lyrics.validate()?;
        }
        self.store_payload(&cache_key(media_id)?, &serde_json::to_string(lookup)?)
    }
}

fn cache_key(media_id: &str) -> Result<String> {
    if media_id.is_empty()
        || media_id.len() > MAX_MEDIA_ID_BYTES
        || !media_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
    {
        return Err(CoreError::Invalid("Invalid media id for lyrics".into()));
    }
    Ok(format!("{CACHE_PREFIX}{media_id}"))
}

enum LrcLine<'a> {
    Timed { starts: Vec<u32>, body: &'a str },
    Offset(i64),
    Metadata,
    Text(&'a str),
}

fn lrc_line(raw: &str) -> LrcLine<'_> {
    let mut rest = raw.trim();
    let mut starts = Vec::new();
    while let Some(after) = rest.strip_prefix('[') {
        let Some(close) = after.find(']') else {
            break;
        };
        let tag = &after[..close];
        if let Some(start) = timestamp_ms(tag) {
            starts.push(start);
            rest = &after[close + 1..];
            continue;
        }
        if starts.is_empty()
            && let Some((key, value)) = tag.split_once(':')
            && metadata_key(key)
        {
            if !key.trim().eq_ignore_ascii_case("offset") {
                return LrcLine::Metadata;
            }
            return LrcLine::Offset(value.trim().parse::<i64>().map_or(0, |offset| {
                offset.clamp(-MAX_LRC_OFFSET_MS, MAX_LRC_OFFSET_MS)
            }));
        }
        break;
    }
    if starts.is_empty() {
        LrcLine::Text(rest)
    } else {
        LrcLine::Timed { starts, body: rest }
    }
}

fn metadata_key(key: &str) -> bool {
    let key = key.trim();
    !key.is_empty()
        && key.len() <= MAX_METADATA_KEY_CHARS
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'#')
}

/// `mm:ss`, `mm:ss.f` (1–6 fraction digits, the first three count) or the
/// `mm:ss:ff` variant, as milliseconds.
fn timestamp_ms(tag: &str) -> Option<u32> {
    let (minutes, rest) = tag.trim().split_once(':')?;
    let (seconds, fraction) = match rest.find(['.', ':']) {
        Some(index) => (&rest[..index], Some(&rest[index + 1..])),
        None => (rest, None),
    };
    let minutes = digits(minutes, 4)?;
    let seconds = digits(seconds, 2)?;
    if seconds > 59 {
        return None;
    }
    let fraction_ms = match fraction {
        None => 0,
        Some(fraction) => {
            digits(fraction, 6)?;
            let kept = &fraction[..fraction.len().min(3)];
            digits(kept, 3)? * 10u32.pow(3 - kept.len() as u32)
        }
    };
    let total = u64::from(minutes) * 60_000 + u64::from(seconds) * 1_000 + u64::from(fraction_ms);
    u32::try_from(total)
        .ok()
        .filter(|total| *total <= MAX_TIMESTAMP_MS)
}

fn digits(text: &str, max_len: usize) -> Option<u32> {
    if text.is_empty() || text.len() > max_len || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The shown text, the word timing and the end of a timed line's body. Word
/// stamps count only on a line with a single time tag (`single_start`),
/// since they are absolute; a word stamped before its line starts with it.
fn timed_body(body: &str, single_start: Option<u32>) -> (String, Vec<LyricWord>, Option<u32>) {
    let parsed = single_start.and_then(|start| Some((start, word_stamps(body)?)));
    let Some((line_start, (words, end_ms))) = parsed else {
        return (clean_text(strip_word_stamps(body)), Vec::new(), None);
    };
    let (text, mut words) = normalize_words(words);
    for word in &mut words {
        word.start_ms = word.start_ms.max(line_start);
    }
    let last_start = words.last().map(|word| word.start_ms);
    let end_ms = end_ms.filter(|end| last_start.is_some_and(|start| *end >= start));
    (text, words, end_ms)
}

/// Words of an enhanced LRC body (`<mm:ss.xx>word <mm:ss.xx>word <mm:ss.xx>`)
/// and the end marked by a final stamp. `None` without valid stamps.
fn word_stamps(body: &str) -> Option<(Vec<LyricWord>, Option<u32>)> {
    let mut leading = String::new();
    let mut words: Vec<LyricWord> = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find('<') {
        let (before, after) = rest.split_at(open);
        append_text(&mut words, &mut leading, before);
        let stamp = after
            .find('>')
            .and_then(|close| timestamp_ms(&after[1..close]).map(|start| (start, close)));
        match stamp {
            Some((start_ms, close)) => {
                words.push(LyricWord {
                    start_ms,
                    text: String::new(),
                });
                rest = &after[close + 1..];
            }
            None => {
                append_text(&mut words, &mut leading, "<");
                rest = &after[1..];
            }
        }
    }
    append_text(&mut words, &mut leading, rest);
    let end_ms = match words.last() {
        Some(last) if last.text.trim().is_empty() => words.pop().map(|last| last.start_ms),
        _ => None,
    };
    let first = words.first_mut()?;
    first.text.insert_str(0, &leading);
    Some((words, end_ms))
}

fn append_text(words: &mut [LyricWord], leading: &mut String, text: &str) {
    match words.last_mut() {
        Some(word) => word.text.push_str(text),
        None => leading.push_str(text),
    }
}

/// Clean word texts, fold space-only words into the previous word, and spell
/// the line. Timing that is not in order, or too fine-grained to keep, is
/// dropped and the line keeps only its text.
fn normalize_words(words: Vec<LyricWord>) -> (String, Vec<LyricWord>) {
    let mut kept: Vec<LyricWord> = Vec::with_capacity(words.len());
    for mut word in words {
        word.text = collapse_spacing(&word.text);
        match kept.last_mut() {
            Some(previous)
                if word.text.trim().is_empty()
                    || (previous.text.ends_with(' ') && word.text.starts_with(' ')) =>
            {
                previous.text.push_str(word.text.trim_start());
            }
            _ if word.text.is_empty() => {}
            _ => kept.push(word),
        }
    }
    if let Some(first) = kept.first_mut() {
        first.text = first.text.trim_start().to_owned();
    }
    if let Some(last) = kept.last_mut() {
        last.text = last.text.trim_end().to_owned();
    }
    kept.retain(|word| !word.text.is_empty());
    let text: String = kept.iter().map(|word| word.text.as_str()).collect();
    let ordered = kept
        .windows(2)
        .all(|pair| pair[0].start_ms <= pair[1].start_ms);
    if text.chars().count() > MAX_LINE_CHARS || kept.len() > MAX_LINE_WORDS || !ordered {
        return (cut(text), Vec::new());
    }
    (text, kept)
}

fn strip_word_stamps(body: &str) -> String {
    let mut text = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(open) = rest.find('<') {
        let (before, after) = rest.split_at(open);
        text.push_str(before);
        match after
            .find('>')
            .filter(|close| timestamp_ms(&after[1..*close]).is_some())
        {
            Some(close) => rest = &after[close + 1..],
            None => {
                text.push('<');
                rest = &after[1..];
            }
        }
    }
    text.push_str(rest);
    text
}

/// Control characters become spaces and runs of spacing collapse to one
/// space; a leading or trailing space is kept so words still separate.
fn collapse_spacing(text: &str) -> String {
    let mut collapsed = String::with_capacity(text.len());
    for character in text.chars() {
        let character = if character.is_control() || character.is_whitespace() {
            ' '
        } else {
            character
        };
        if !(character == ' ' && collapsed.ends_with(' ')) {
            collapsed.push(character);
        }
    }
    collapsed
}

fn clean_text(text: impl AsRef<str>) -> String {
    cut(collapse_spacing(text.as_ref()).trim().to_owned())
}

fn cut(text: String) -> String {
    match text.char_indices().nth(MAX_LINE_CHARS) {
        Some((index, _)) => text[..index].trim_end().to_owned(),
        None => text,
    }
}

fn sidecar_lyrics(media: &Path) -> Option<Lyrics> {
    let path = sidecar_path(media)?;
    let mut bytes = Vec::new();
    File::open(&path)
        .ok()?
        .take(MAX_LYRICS_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_LYRICS_BYTES {
        return None;
    }
    Lyrics::parse(&decode_text(&bytes))
}

/// The `.lrc` next to `media`, resolved through links; it must be a regular
/// file in the same folder as the resolved media file.
fn sidecar_path(media: &Path) -> Option<PathBuf> {
    let media = media.canonicalize().ok()?;
    let sidecar = media
        .with_extension(SIDECAR_EXTENSION)
        .canonicalize()
        .ok()?;
    (sidecar.parent() == media.parent() && sidecar.is_file()).then_some(sidecar)
}

/// UTF-8 (with or without a byte order mark), UTF-16 with a byte order mark,
/// and otherwise Latin-1, which older lyrics editors still write.
fn decode_text(bytes: &[u8]) -> Cow<'_, str> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest);
    }
    let utf16 = |rest: &[u8], decode: fn([u8; 2]) -> u16| {
        let units = rest.chunks_exact(2).map(|pair| decode([pair[0], pair[1]]));
        char::decode_utf16(units)
            .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect::<String>()
    };
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return Cow::Owned(utf16(rest, u16::from_le_bytes));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return Cow::Owned(utf16(rest, u16::from_be_bytes));
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => Cow::Owned(bytes.iter().map(|&byte| char::from(byte)).collect()),
    }
}

fn embedded_lyrics(path: &Path) -> Option<Lyrics> {
    use lofty::{config::ParseOptions, file::TaggedFileExt, prelude::ItemKey, probe::Probe};
    let tagged = Probe::open(path)
        .ok()?
        .options(ParseOptions::new().read_properties(false))
        .guess_file_type()
        .ok()?
        .read()
        .ok()?;
    let keys = [
        ItemKey::Lyrics,
        ItemKey::Unknown(UNSYNCED_LYRICS_KEY.to_owned()),
    ];
    let texts: Vec<Lyrics> = tagged
        .tags()
        .iter()
        .flat_map(|tag| keys.iter().filter_map(|key| tag.get_string(key)))
        .filter_map(Lyrics::parse)
        .collect();
    let synced = texts.iter().find(|lyrics| lyrics.is_synced()).cloned();
    synced
        .or_else(|| sylt_lyrics(path))
        .or_else(|| texts.into_iter().next())
}

/// Synchronized lyrics of an MP3's ID3v2 SYLT frame timed in milliseconds.
/// Frames timed in MPEG frames are ignored: their unit depends on the stream.
fn sylt_lyrics(path: &Path) -> Option<Lyrics> {
    use lofty::{
        config::ParseOptions,
        file::AudioFile,
        id3::v2::{Frame, FrameId, SyncTextContentType, SynchronizedTextFrame, TimestampFormat},
        mpeg::MpegFile,
    };
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mp3"))
    {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let mpeg = MpegFile::read_from(&mut file, ParseOptions::new().read_properties(false)).ok()?;
    let Frame::Binary(binary) = mpeg.id3v2()?.get(&FrameId::Valid(Cow::Borrowed("SYLT")))? else {
        return None;
    };
    let frame = SynchronizedTextFrame::parse(&binary.data, binary.flags()).ok()?;
    let lyrics = matches!(
        frame.content_type,
        SyncTextContentType::Lyrics | SyncTextContentType::Other
    );
    (lyrics && frame.timestamp_format == TimestampFormat::MS)
        .then(|| Lyrics::from_timed_entries(&frame.content))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lyrics: &Lyrics) -> Vec<&str> {
        lyrics
            .lines()
            .iter()
            .map(|line| line.text.as_str())
            .collect()
    }

    fn starts(lyrics: &Lyrics) -> Vec<u32> {
        lyrics.lines().iter().map(|line| line.start_ms).collect()
    }

    #[test]
    fn lrc_lines_are_timed_sorted_repeated_and_shifted_by_the_offset() {
        let lyrics = Lyrics::parse(
            "\u{feff}[ar: Artist]\r\n[ti:Song]\n[offset:+500]\n[00:12.50]Second\n[00:01.00][01:02.3]Chorus  line\n[00:20.00]\n[00:21.00]\n[1:00.007] Last\nuntimed",
        )
        .unwrap();
        assert!(lyrics.is_synced());
        assert_eq!(
            texts(&lyrics),
            ["Chorus line", "Second", "", "Last", "Chorus line"]
        );
        assert_eq!(starts(&lyrics), [500, 12_000, 19_500, 59_507, 61_800]);
        assert!(!lyrics.has_word_timing());
    }

    #[test]
    fn enhanced_lrc_keeps_word_timing_and_the_line_end() {
        let lyrics = Lyrics::parse(
            "[00:10.00]<00:10.00>Hel<00:10.40>lo <00:11.00>world<00:12.50>\n[00:13.00]Next",
        )
        .unwrap();
        let line = &lyrics.lines()[0];
        assert_eq!(line.text, "Hello world");
        assert_eq!(
            line.words
                .iter()
                .map(|word| (word.start_ms, word.text.as_str()))
                .collect::<Vec<_>>(),
            [(10_000, "Hel"), (10_400, "lo "), (11_000, "world")]
        );
        assert_eq!(line.end_ms, Some(12_500));
        assert!(lyrics.has_word_timing());
        let disordered = Lyrics::parse("[00:10.00]<00:11.00>b <00:10.50>a").unwrap();
        assert_eq!(disordered.lines()[0].text, "b a");
        assert!(disordered.lines()[0].words.is_empty());
    }

    #[test]
    fn plain_text_keeps_stanzas_and_ignores_metadata() {
        let lyrics =
            Lyrics::parse("[ar:Someone]\n\nFirst line\n[Chorus]\n\n\n\nSecond\tline\n\n").unwrap();
        assert!(!lyrics.is_synced());
        assert_eq!(
            texts(&lyrics),
            ["First line", "[Chorus]", "", "Second line"]
        );
        assert!(Lyrics::parse("[ti:Only metadata]\n\n").is_none());
        assert!(Lyrics::parse("[00:01.00]\n[00:02.00]").is_none());
        assert!(Lyrics::parse(&"a".repeat(MAX_LYRICS_BYTES + 1)).is_none());
    }

    #[test]
    fn malformed_tags_never_panic_or_escape_the_bounds() {
        for text in [
            "[",
            "[]",
            "[00:61.00]Bad seconds",
            "[99999:00.00]Too late",
            "[00:01.00]<00:0",
            "[00:01.00]<>a<",
            "[00:01.00]<00:01.00>",
            "[offset:abc]\n[00:01.00]x",
            "[00:01.000000001]x",
        ] {
            if let Some(lyrics) = Lyrics::parse(text) {
                lyrics.validate().unwrap();
            }
        }
        let long =
            Lyrics::parse(&format!("[00:01.00]{}", "é".repeat(MAX_LINE_CHARS + 50))).unwrap();
        assert_eq!(long.lines()[0].text.chars().count(), MAX_LINE_CHARS);
        let many = (0..MAX_LYRICS_LINES + 10)
            .map(|index| format!("[{:02}:{:02}.00]line {index}", index / 60, index % 60))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            Lyrics::parse(&many).unwrap().lines().len(),
            MAX_LYRICS_LINES
        );
    }

    #[test]
    fn timed_entries_group_syllables_on_line_breaks() {
        let grouped = Lyrics::from_timed_entries(&[
            (1_000, "Hel".into()),
            (1_300, "lo".into()),
            (2_000, "\nAgain".into()),
        ])
        .unwrap();
        assert_eq!(texts(&grouped), ["Hello", "Again"]);
        assert_eq!(grouped.lines()[0].words.len(), 2);
        let lines =
            Lyrics::from_timed_entries(&[(1_000, "First line".into()), (3_000, "Second".into())])
                .unwrap();
        assert_eq!(starts(&lines), [1_000, 3_000]);
    }

    #[test]
    fn stored_lyrics_are_validated_again() {
        let lyrics = Lyrics::parse("[00:01.00]One\n[00:02.00]Two").unwrap();
        let json = serde_json::to_string(&lyrics).unwrap();
        assert_eq!(serde_json::from_str::<Lyrics>(&json).unwrap(), lyrics);
        for forged in [
            r#"{"synced":true,"lines":[{"start_ms":2000,"text":"b"},{"start_ms":1000,"text":"a"}]}"#,
            r#"{"synced":false,"lines":[{"start_ms":5,"text":"a"}]}"#,
            r#"{"synced":true,"lines":[{"start_ms":1,"text":"a\nb"}]}"#,
            r#"{"synced":true,"lines":[{"start_ms":1,"text":"ab","words":[{"start_ms":1,"text":"a"}]}]}"#,
            r#"{"synced":true,"lines":[]}"#,
        ] {
            assert!(serde_json::from_str::<Lyrics>(forged).is_err(), "{forged}");
        }
    }

    #[test]
    fn queries_and_candidates_are_bounded() {
        assert!(LyricsQuery::new(" ", "Artist", "", 10.0).is_err());
        assert!(LyricsQuery::new("Song", "A\u{7}", "", 10.0).is_err());
        assert!(LyricsQuery::new("Song", "Artist", "", f64::NAN).is_err());
        let query = LyricsQuery::new(" Song ", "Artist", "", 0.0).unwrap();
        assert_eq!(query.title(), "Song");
        let candidate = LyricsCandidate::from_record(
            "Song",
            "Artist",
            "Album",
            f64::INFINITY,
            false,
            Lyrics::parse("not synced at all"),
            Lyrics::parse("Plain words"),
        )
        .unwrap();
        assert_eq!(candidate.duration, 0.0);
        assert!(candidate.synced.is_none());
        assert!(candidate.plain.is_some());
        assert!(LyricsCandidate::from_record("Song", "", "", 1.0, false, None, None).is_none());
        assert!(LyricsCandidate::from_record("Song", "", "", 1.0, true, None, None).is_some());
    }

    #[test]
    fn text_decoding_accepts_byte_order_marks_and_latin1() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFcanci\xC3\xB3n"), "canción");
        assert_eq!(decode_text(b"\xFF\xFEa\0\xF3\0"), "aó");
        assert_eq!(decode_text(b"\xFE\xFF\0a\0\xF3"), "aó");
        assert_eq!(decode_text(b"canci\xF3n"), "canción");
    }

    #[test]
    fn library_tracks_read_sidecar_files_and_lookups_are_cached() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let track = core.import_bytes("song.wav", &[1, 2, 3, 4]).unwrap();
        assert_eq!(core.local_lyrics(&track.id).unwrap(), None);
        std::fs::write(track.path.with_extension("lrc"), "[00:01.00]From the file").unwrap();
        let (lyrics, source) = core.local_lyrics(&track.id).unwrap().unwrap();
        assert_eq!(source, LyricsSource::Sidecar);
        assert_eq!(texts(&lyrics), ["From the file"]);
        assert!(core.local_lyrics("unknown-track").is_err());

        assert!(core.cached_lyrics(&track.id).unwrap().is_none());
        let found = LyricsLookup::Found {
            lyrics,
            source: LyricsSource::Lrclib,
        };
        core.store_lyrics(&track.id, &found).unwrap();
        core.store_lyrics("youtube:aqz-KE-bpKQ", &LyricsLookup::Missing)
            .unwrap();
        assert_eq!(
            core.cached_lyrics(&track.id).unwrap().unwrap().lookup,
            found
        );
        assert_eq!(
            core.cached_lyrics("youtube:aqz-KE-bpKQ")
                .unwrap()
                .unwrap()
                .lookup,
            LyricsLookup::Missing
        );
        assert!(core.store_lyrics("bad id", &LyricsLookup::Missing).is_err());
        core.store_payload(
            &cache_key("corrupt").unwrap(),
            r#"{"status":"found","lyrics":{"synced":true,"lines":[]},"source":"lrclib"}"#,
        )
        .unwrap();
        assert!(core.cached_lyrics("corrupt").unwrap().is_none());

        core.clear_listening_history().unwrap();
        assert!(core.cached_lyrics(&track.id).unwrap().is_none());
    }

    #[test]
    fn sidecar_links_outside_the_media_folder_are_refused() {
        let folder = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let media = folder.path().join("song.mp3");
        std::fs::write(&media, b"audio").unwrap();
        let target = outside.path().join("other.lrc");
        std::fs::write(&target, "[00:01.00]Elsewhere").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&target, folder.path().join("song.lrc")).unwrap();
            assert!(sidecar_lyrics(&media).is_none());
        }
    }

    #[test]
    fn mp3_sylt_frames_in_milliseconds_are_read() {
        use lofty::{
            TextEncoding,
            config::WriteOptions,
            id3::v2::{
                BinaryFrame, Frame, FrameId, Id3v2Tag, SyncTextContentType, SynchronizedTextFrame,
                TimestampFormat,
            },
            tag::TagExt,
        };
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("sylt.mp3");
        let frame = SynchronizedTextFrame::new(
            TextEncoding::UTF8,
            *b"spa",
            TimestampFormat::MS,
            SyncTextContentType::Lyrics,
            None,
            vec![(1_500, "Hola".into()), (4_000, "Adiós".into())],
        );
        let mut tag = Id3v2Tag::new();
        tag.insert(Frame::Binary(BinaryFrame::new(
            FrameId::Valid(Cow::Borrowed("SYLT")),
            frame.as_bytes().unwrap(),
        )));
        let mut bytes = Vec::new();
        tag.dump_to(&mut bytes, WriteOptions::default()).unwrap();
        // One silent MPEG-1 Layer III frame (128 kbit/s, 44.1 kHz) after the tag.
        bytes.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x64]);
        bytes.resize(bytes.len() + 413, 0);
        std::fs::write(&path, bytes).unwrap();
        let lyrics = embedded_lyrics(&path).unwrap();
        assert_eq!(texts(&lyrics), ["Hola", "Adiós"]);
        assert_eq!(starts(&lyrics), [1_500, 4_000]);
    }
}
