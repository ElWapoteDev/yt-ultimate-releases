//! Lyricsfile 1.0 documents, YAML with line and word timing in integer
//! milliseconds (draft specification:
//! <https://github.com/tranxuanthang/lyricsfile/blob/main/SPECIFICATION.md>).

use super::{
    LyricLine, LyricWord, Lyrics, MAX_LYRICS_BYTES, MAX_TIMESTAMP_MS, clean_text, normalize_words,
};
use serde::Deserialize;

/// The only version read: the draft forbids reading another one as 1.0.
const VERSION: &str = "1.0";
/// Deepest nesting parsed. The document, its lines, a line, its words and a
/// word take five levels; the rest leaves room for fields this parser skips.
const MAX_DEPTH: usize = 8;

/// The fields read from a document. Every field is optional, so a missing
/// one only loses what it carries; a value of another type refuses the
/// document, which the draft calls a structural error.
#[derive(Deserialize)]
struct Document {
    version: Option<String>,
    lines: Option<Vec<Line>>,
}

#[derive(Deserialize)]
struct Line {
    text: Option<String>,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    words: Option<Vec<Word>>,
}

#[derive(Deserialize)]
struct Word {
    text: Option<String>,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
}

impl Lyrics {
    /// Parse a Lyricsfile 1.0 document into synced lyrics, with word timing
    /// where the document has it. A line without a start within the media
    /// bounds is dropped. A line keeps word timing when every word has a
    /// start and the words spell clean text in time order within
    /// `MAX_LINE_WORDS`; it then shows the words' spelling and ends where its
    /// last word, or else the line, ends. Otherwise it keeps its own text and
    /// start. The global `offset_ms` is ignored while the draft leaves its
    /// meaning open. `None` when `text` exceeds `MAX_LYRICS_BYTES`, is not a
    /// single version 1.0 YAML document within the parser's budget (no
    /// anchors, aliases or duplicate keys, nesting within `MAX_DEPTH`, values
    /// of the specified types), or has no timed line with text.
    pub fn from_lyricsfile(text: &str) -> Option<Self> {
        if text.len() > MAX_LYRICS_BYTES {
            return None;
        }
        let options = serde_saphyr::options! {
            budget: serde_saphyr::budget! {
                max_documents: 1,
                max_depth: MAX_DEPTH,
                max_anchors: 0,
                max_aliases: 0,
            },
        };
        let document: Document = serde_saphyr::from_str_with_options(text, options).ok()?;
        if document.version.as_deref() != Some(VERSION) {
            return None;
        }
        let lines = document
            .lines
            .unwrap_or_default()
            .into_iter()
            .filter_map(Line::into_lyric_line)
            .collect();
        Self::timed(lines, 0)
    }
}

impl Line {
    fn into_lyric_line(self) -> Option<LyricLine> {
        let start_ms = millis(self.start_ms)?;
        let timed = self
            .words
            .and_then(|words| timed_words(words, start_ms, millis(self.end_ms)));
        Some(match timed {
            Some((text, words, end_ms)) => LyricLine {
                start_ms,
                text,
                words,
                end_ms,
            },
            None => LyricLine {
                start_ms,
                text: clean_text(self.text.unwrap_or_default()),
                words: Vec::new(),
                end_ms: None,
            },
        })
    }
}

/// The spelling, word timing and end of a line whose words all have a start.
/// A word stamped before its line starts with it, like in enhanced LRC.
/// `None` when a word has no start or no timing survives normalizing.
fn timed_words(
    words: Vec<Word>,
    line_start: u32,
    line_end: Option<u32>,
) -> Option<(String, Vec<LyricWord>, Option<u32>)> {
    let end_ms = words
        .last()
        .and_then(|word| millis(word.end_ms))
        .or(line_end);
    let words = words
        .into_iter()
        .map(|word| {
            Some(LyricWord {
                start_ms: millis(word.start_ms)?,
                text: word.text.unwrap_or_default(),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let (text, mut words) = normalize_words(words);
    for word in &mut words {
        word.start_ms = word.start_ms.max(line_start);
    }
    let last_start = words.last()?.start_ms;
    Some((text, words, end_ms.filter(|end| *end >= last_start)))
}

/// A timestamp within the media bounds; a negative or later one is absent.
fn millis(value: Option<i64>) -> Option<u32> {
    value
        .and_then(|ms| u32::try_from(ms).ok())
        .filter(|ms| *ms <= MAX_TIMESTAMP_MS)
}

#[cfg(test)]
mod tests {
    use super::super::{MAX_LINE_CHARS, MAX_LINE_WORDS, MAX_LYRICS_LINES};
    use super::*;

    /// Words of a line as (start, text) pairs.
    fn words(line: &LyricLine) -> Vec<(u32, &str)> {
        line.words
            .iter()
            .map(|word| (word.start_ms, word.text.as_str()))
            .collect()
    }

    #[test]
    fn documents_keep_word_timing_and_line_ends() {
        let lyrics = Lyrics::from_lyricsfile(
            "version: '1.0'
metadata:
  title: Example Song
  artist: Example Artist
  duration_ms: 200000
  offset_ms: 5000
  language: en
lines:
- text: First example line
  start_ms: 1000
  end_ms: 2600
  words:
  - text: 'First '
    start_ms: 1000
  - text: 'example '
    start_ms: 1400
  - text: line
    start_ms: 2000
- text: Second line
  start_ms: 3000
  words:
  - text: 'Second '
    start_ms: 3000
  - {text: line, start_ms: 3500, end_ms: 4100}
- text: '  Only   line   timing '
  start_ms: 5000
  end_ms: 6000
- text: ''
  start_ms: 7000
- text: 99
  start_ms: 8000
  future_field: {nested: [1, 2]}
plain: |
  First example line
  Second line
",
        )
        .unwrap();
        assert!(lyrics.is_synced());
        assert!(lyrics.has_word_timing());
        let lines = lyrics.lines();
        assert_eq!(
            lines
                .iter()
                .map(|line| (line.start_ms, line.text.as_str()))
                .collect::<Vec<_>>(),
            [
                (1_000, "First example line"),
                (3_000, "Second line"),
                (5_000, "Only line timing"),
                (7_000, ""),
                (8_000, "99"),
            ]
        );
        assert_eq!(
            words(&lines[0]),
            [(1_000, "First "), (1_400, "example "), (2_000, "line")]
        );
        assert_eq!(lines[0].end_ms, Some(2_600));
        assert_eq!(lines[1].end_ms, Some(4_100));
        assert!(lines[2].words.is_empty());
        assert_eq!(lines[2].end_ms, None);
    }

    #[test]
    fn words_spell_the_line_and_start_with_it() {
        let lyrics = Lyrics::from_lyricsfile(
            "version: '1.0'
lines:
- text: Spelled elsewhere
  start_ms: 2000
  end_ms: 1500
  words:
  - {text: 'Early ', start_ms: 1500}
  - {text: ' ', start_ms: 1800}
  - {text: 'words', start_ms: 2500}
- start_ms: 4000
  words:
  - {text: 'No ', start_ms: 4000}
  - {text: text, start_ms: 4200}
",
        )
        .unwrap();
        let lines = lyrics.lines();
        assert_eq!(lines[0].text, "Early words");
        assert_eq!(words(&lines[0]), [(2_000, "Early "), (2_500, "words")]);
        assert_eq!(lines[0].end_ms, None);
        assert_eq!(lines[1].text, "No text");
    }

    #[test]
    fn unusable_timing_loses_only_what_it_affects() {
        let lyrics = Lyrics::from_lyricsfile(
            "version: '1.0'
lines:
- {text: Before the start, start_ms: -5}
- {text: After the media, start_ms: 999999999999}
- {text: Missing start}
- text: A word without a start
  start_ms: 1000
  words:
  - {text: 'A ', start_ms: 1000}
  - {text: 'word '}
  - {text: without a start, start_ms: 1600}
- text: Words out of order
  start_ms: 3000
  words:
  - {text: 'Words ', start_ms: 3500}
  - {text: 'out of order', start_ms: 3100}
- text: Negative word
  start_ms: 4000
  words:
  - {text: 'Negative ', start_ms: 4000}
  - {text: word, start_ms: -1}
",
        )
        .unwrap();
        assert_eq!(
            lyrics
                .lines()
                .iter()
                .map(|line| (line.start_ms, line.text.as_str(), line.words.len()))
                .collect::<Vec<_>>(),
            [
                (1_000, "A word without a start", 0),
                (3_000, "Words out of order", 0),
                (4_000, "Negative word", 0),
            ]
        );
        assert!(!lyrics.has_word_timing());
    }

    #[test]
    fn documents_of_another_shape_are_refused() {
        let line = "lines:\n- {text: Line, start_ms: 1000}\n";
        assert!(Lyrics::from_lyricsfile(&format!("version: '1.0'\n{line}")).is_some());
        for text in [
            String::new(),
            "not: [valid".into(),
            "- a sequence".into(),
            line.into(),
            format!("version: '2.0'\n{line}"),
            format!("version: 1\n{line}"),
            "version: '1.0'\nlines:\n- {text: Line, start_ms: soon}\n".into(),
            "version: '1.0'\nlines:\n- {text: Line, start_ms: 1.5}\n".into(),
            "version: '1.0'\nlines: {text: Line}\n".into(),
            format!("version: '1.0'\nversion: '1.0'\n{line}"),
            format!("version: '1.0'\n{line}---\nversion: '1.0'\n"),
            "version: '1.0'\nlines:\n- &line {text: Line, start_ms: 1000}\n- *line\n".into(),
            format!(
                "version: '1.0'\nextra: {}1{}\n{line}",
                "[".repeat(10),
                "]".repeat(10)
            ),
            "version: '1.0'\nlines:\n- {text: '', start_ms: 1000}\n".into(),
            format!("version: '1.0'\n{line}{}", "#".repeat(MAX_LYRICS_BYTES)),
        ] {
            assert!(Lyrics::from_lyricsfile(&text).is_none(), "{text:.80}");
        }
    }

    #[test]
    fn lines_and_words_are_cut_to_the_bounds() {
        let mut text = String::from("version: '1.0'\nlines:\n");
        for index in 0..MAX_LYRICS_LINES + 5 {
            text.push_str(&format!("- {{text: line {index}, start_ms: {index}}}\n"));
        }
        let crowded = (0..=MAX_LINE_WORDS)
            .map(|index| format!("{{text: 'w ', start_ms: {}}}", 2_000 + index))
            .collect::<Vec<_>>()
            .join(", ");
        text.push_str(&format!(
            "- {{text: Crowded, start_ms: 0, words: [{crowded}]}}\n"
        ));
        text.push_str(&format!(
            "- {{text: {}, start_ms: 1, words: [{{text: {}, start_ms: 1}}]}}\n",
            "a".repeat(MAX_LINE_CHARS + 20),
            "a".repeat(MAX_LINE_CHARS + 20)
        ));
        let lyrics = Lyrics::from_lyricsfile(&text).unwrap();
        let lines = lyrics.lines();
        assert_eq!(lines.len(), MAX_LYRICS_LINES);
        assert_eq!(lines[0].text, "line 0");
        assert_eq!(lines[1].text, "Crowded");
        assert!(lines[1].words.is_empty());
        assert_eq!(lines[3].text.chars().count(), MAX_LINE_CHARS);
        assert!(lines[3].words.is_empty());
    }
}
