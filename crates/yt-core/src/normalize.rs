//! Song titles and artist names across uploads of the same music: comparison
//! keys (discovery, lyrics matching) and the cleaned display forms that
//! catalogs are searched with (lyrics lookups).

use unicode_normalization::char::{decompose_compatible, is_combining_mark};

/// Channel decorations that do not change who the artist is.
const ARTIST_SUFFIXES: &[&str] = &[" - topic", "vevo", " official", "official"];
/// Words that mark a bracketed title segment as packaging, not content.
const TITLE_NOISE: &[&str] = &[
    "official",
    "video",
    "audio",
    "lyric",
    "lyrics",
    "letra",
    "hd",
    "4k",
    "visualizer",
    "mv",
    "remaster",
    "remastered",
    "clip",
    "videoclip",
];
/// Markers that start a featured-artist tail of a title.
const FEATURE_MARKERS: &[&str] = &[" ft.", " ft ", " feat.", " feat ", " featuring "];
/// Separators between a leading performer and the song in upload titles.
const PERFORMER_SEPARATORS: &[&str] = &[" - ", " – ", " — "];

/// A song title split into its parts, with packaging removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongTitle {
    /// The «Artist - » prefix of an upload title, when present.
    pub performer: Option<String>,
    /// The song name: bracketed packaging such as «(Official Video)» and a
    /// featured-artist tail are removed; meaningful brackets stay.
    pub song: String,
}

/// Split `title` into performer and song, keeping the original letters, so
/// the result can be shown or sent to a catalog search.
pub fn song_title(title: &str) -> SongTitle {
    let unpacked = strip_packaging(title);
    let without_feature = FEATURE_MARKERS
        .iter()
        .filter_map(|marker| find_ascii_case_insensitive(&unpacked, marker))
        .min()
        .map_or(unpacked.as_str(), |index| &unpacked[..index]);
    let split = PERFORMER_SEPARATORS
        .iter()
        .filter_map(|separator| {
            without_feature
                .find(separator)
                .map(|index| (index, separator.len()))
        })
        .min();
    let (performer, song) = match split {
        Some((index, length)) => (
            Some(without_feature[..index].trim()).filter(|performer| !performer.is_empty()),
            &without_feature[index + length..],
        ),
        None => (None, without_feature),
    };
    SongTitle {
        performer: performer.map(collapse_whitespace),
        song: collapse_whitespace(song),
    }
}

/// `name` without channel decorations such as « - Topic», «VEVO» or
/// «Official», keeping the original letters.
pub fn clean_artist(name: &str) -> String {
    let mut name = name.trim();
    for suffix in ARTIST_SUFFIXES {
        if let Some(stripped) = strip_suffix_ascii_case_insensitive(name, suffix) {
            name = stripped;
        }
    }
    collapse_whitespace(name)
}

/// Key under which uploads of one artist group together, for example
/// «Enrique Iglesias», «EnriqueIglesiasVEVO» and «Enrique Iglesias - Topic».
pub fn artist_key(name: &str) -> String {
    alphanumeric(&clean_artist(name).to_lowercase())
}

/// Key under which uploads of one song collapse: packaging in brackets,
/// featured artists and a leading «Artist - » prefix are ignored.
pub fn title_key(title: &str) -> String {
    alphanumeric(&song_title(title).song.to_lowercase())
}

/// `title` without bracketed packaging. An unclosed bracket is content, not
/// packaging: what followed it is kept.
fn strip_packaging(title: &str) -> String {
    let mut kept = String::with_capacity(title.len());
    let mut segment = String::new();
    let mut depth = 0usize;
    for character in title.chars() {
        match character {
            '(' | '[' => {
                depth += 1;
                if depth == 1 {
                    segment.clear();
                    continue;
                }
            }
            ')' | ']' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if !is_noise(&segment) {
                        kept.push(' ');
                        kept.push_str(&segment);
                    }
                    continue;
                }
            }
            _ => {}
        }
        if depth == 0 {
            kept.push(character);
        } else {
            segment.push(character);
        }
    }
    if depth > 0 {
        kept.push(' ');
        kept.push_str(&segment);
    }
    kept
}

fn is_noise(segment: &str) -> bool {
    segment
        .split(|character: char| !character.is_alphanumeric())
        .any(|word| TITLE_NOISE.contains(&word.to_lowercase().as_str()))
}

// Needles are ASCII, so byte comparison cannot split a UTF-8 sequence and the
// returned index is always a character boundary.
fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

fn strip_suffix_ascii_case_insensitive<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    let start = text.len().checked_sub(suffix.len())?;
    (text.is_char_boundary(start)
        && text.as_bytes()[start..].eq_ignore_ascii_case(suffix.as_bytes()))
    .then(|| &text[..start])
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The apostrophe every apostrophe-like character folds to.
pub const FOLDED_APOSTROPHE: char = '\'';
/// Characters typed for an apostrophe. The acute accent is included because
/// Spanish keyboards produce «´» where «'» was meant, and NFKD would otherwise
/// turn it into a space.
const APOSTROPHE_VARIANTS: [char; 5] = ['\u{2019}', '\u{2018}', '\u{02BC}', '\u{00B4}', '\u{0060}'];
/// Lowercase Latin letters without a Unicode decomposition, spelled the way
/// a keyboard without them writes them.
const UNDECOMPOSABLE_LETTERS: [(char, &str); 9] = [
    ('ß', "ss"),
    ('æ', "ae"),
    ('œ', "oe"),
    ('ø', "o"),
    ('đ', "d"),
    ('ð', "d"),
    ('ł', "l"),
    ('ı', "i"),
    ('þ', "th"),
];

/// Folds `text` for comparison: compatibility decomposition (NFKD), combining
/// marks removed, lowercase, apostrophe variants written as `'`, and every
/// run of whitespace collapsed to one space with none at either end.
///
/// «Canción» (composed or decomposed) and «CANCION» fold to `cancion`,
/// «Ñandú» to `nandu`, «Pingüino» to `pinguino`, «ﬁesta» to `fiesta`,
/// «Røyksopp» to `royksopp` and «Straße» to `strasse`. Punctuation is kept. Total: never fails and never panics. Time O(len).
pub fn fold(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    fold_into(text, &mut folded);
    folded
}

/// [`fold`] into `folded`, replacing its contents, so loops over many texts
/// reuse one buffer.
pub fn fold_into(text: &str, folded: &mut String) {
    folded.clear();
    if is_plain_ascii(text) {
        // The common case: copy and lowercase in bulk.
        folded.push_str(text);
        folded.make_ascii_lowercase();
        return;
    }
    let mut writer = Folder {
        folded,
        space_pending: false,
    };
    for character in text.chars().map(unify_apostrophe) {
        if character.is_ascii() {
            writer.push(character.to_ascii_lowercase());
        } else {
            // NFKD only reorders combining marks, and all of them are dropped,
            // so decomposing one character at a time equals decomposing the text.
            decompose_compatible(character, |part| writer.push_lowercase(part));
        }
    }
}

/// ASCII without apostrophe variants whose only whitespace is single spaces
/// between words: its fold is its lowercase.
fn is_plain_ascii(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.first() != Some(&b' ')
        && bytes.last() != Some(&b' ')
        && !text.contains("  ")
        && bytes.iter().all(|byte| {
            let character = char::from(*byte);
            byte.is_ascii()
                && unify_apostrophe(character) == character
                && (*byte == b' ' || !character.is_whitespace())
        })
}

fn unify_apostrophe(character: char) -> char {
    if APOSTROPHE_VARIANTS.contains(&character) {
        FOLDED_APOSTROPHE
    } else {
        character
    }
}

/// Writes folded characters, collapsing whitespace as the text grows.
struct Folder<'a> {
    folded: &'a mut String,
    space_pending: bool,
}

impl Folder<'_> {
    fn push_lowercase(&mut self, character: char) {
        for lower in character.to_lowercase() {
            if is_combining_mark(lower) {
                continue;
            }
            match UNDECOMPOSABLE_LETTERS
                .iter()
                .find(|(letter, _)| *letter == lower)
            {
                Some((_, spelled)) => spelled.chars().for_each(|letter| self.push(letter)),
                None => self.push(lower),
            }
        }
    }

    fn push(&mut self, character: char) {
        if character.is_whitespace() {
            self.space_pending = !self.folded.is_empty();
            return;
        }
        if self.space_pending {
            self.folded.push(' ');
            self.space_pending = false;
        }
        self.folded.push(character);
    }
}

/// The [`fold`]ed words of `text` separated by single spaces, without the
/// punctuation between them: «Tití Me Preguntó (Video)» gives
/// `titi me pregunto video`. Names that differ only in accents, case,
/// spacing or punctuation give equal words.
pub fn fold_words(text: &str) -> String {
    fold(text)
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether `phrase` appears in `text` as consecutive whole words, comparing
/// their [`fold_words`]; an empty phrase never matches. «Ciudad de México»
/// contains «mexico», «Songbird» does not contain «song».
pub fn contains_words(text: &str, phrase: &str) -> bool {
    let phrase = fold_words(phrase);
    !phrase.is_empty() && format!(" {} ", fold_words(text)).contains(&format!(" {phrase} "))
}

fn alphanumeric(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_ignores_accents_case_and_spacing() {
        assert_eq!(fold("  Canción   de\tAMOR \n"), "cancion de amor");
        assert_eq!(fold("Ñandú"), "nandu");
        assert_eq!(fold("Pingüino ÉLAN"), "pinguino elan");
        assert_eq!(fold("cancio\u{301}n"), "cancion");
        assert_eq!(fold("CANCIÓN"), fold("canción"));
        assert_eq!(fold("Peso\u{a0}\u{2003}Pluma"), "peso pluma");
        assert_eq!(fold(""), "");
        assert_eq!(fold(" \t\n "), "");
    }

    #[test]
    fn folding_spells_compatibility_forms_and_special_letters() {
        assert_eq!(fold("ﬁesta"), "fiesta");
        assert_eq!(fold("ＡＢＣ１２"), "abc12");
        assert_eq!(fold("Straße"), "strasse");
        assert_eq!(fold("Røyksopp"), "royksopp");
        assert_eq!(fold("Łódź"), "lodz");
        assert_eq!(fold("İstanbul"), "istanbul");
        assert_eq!(fold("Don’t Don´t Don't"), "don't don't don't");
        assert_eq!(fold("AC/DC (Live)"), "ac/dc (live)");
    }

    /// Folding through a whole-text NFKD pass, the definition `fold` must meet.
    fn reference_fold(text: &str) -> String {
        use unicode_normalization::UnicodeNormalization;
        let mut folded = String::new();
        let mut writer = Folder {
            folded: &mut folded,
            space_pending: false,
        };
        for character in text.chars().map(unify_apostrophe).nfkd() {
            writer.push_lowercase(character);
        }
        folded
    }

    #[test]
    fn per_character_folding_equals_whole_text_decomposition() {
        let samples = [
            "Bailando (Remix) - Enrique Iglesias feat. Descemer Bueno",
            " Leading, trailing ",
            "double  space\ttab\x0bvertical`grave",
            "",
            "Canción ÑANDÚ Pingüino",
            "a\u{323}\u{301}e\u{301}\u{323} o\u{308}\u{304}",
            "ﬁﬂ ＡＢＣ ¹²³ ½ ℌ ㎏ Ⅻ",
            "한국어 노래 가나다 ｶﾀｶﾅ が ぱ",
            "Ǆemal ǅ ǆ İstanbul ΆΈΉ Ёлка ǰ",
            "  mixed\u{a0}spaces\u{2003}and\ttabs\n",
            "Don´t ’quote’ ʼok` Straße ØÆŒ",
        ];
        for sample in samples {
            assert_eq!(fold(sample), reference_fold(sample), "{sample}");
        }
        // The equivalence needs every reorderable character to be a mark.
        let reorderable_non_marks = (0..=u32::from(char::MAX))
            .filter_map(char::from_u32)
            .filter(|character| {
                unicode_normalization::char::canonical_combining_class(*character) != 0
                    && !is_combining_mark(*character)
            })
            .count();
        assert_eq!(reorderable_non_marks, 0);
    }

    #[test]
    fn phrases_match_whole_words_ignoring_accents_case_and_punctuation() {
        assert_eq!(
            fold_words("Tití Me Preguntó (Video) | Un Verano"),
            "titi me pregunto video un verano"
        );
        assert_eq!(fold_words(" -- "), "");
        assert!(contains_words("Ciudad de México, CDMX", "mexico"));
        assert!(contains_words(
            "Bad Bunny - Tití Me Preguntó (Video)",
            "Titi me pregunto"
        ));
        assert!(!contains_words("Songbird live session", "Song"));
        assert!(!contains_words("Me preguntó Tití", "Titi me pregunto"));
        assert!(!contains_words("Anything", "  ()  "));
    }

    #[test]
    fn artist_uploads_share_one_key() {
        let key = artist_key("Enrique Iglesias");
        assert_eq!(key, "enriqueiglesias");
        assert_eq!(artist_key("EnriqueIglesiasVEVO"), key);
        assert_eq!(artist_key("Enrique Iglesias - Topic"), key);
        assert_eq!(artist_key("  ENRIQUE IGLESIAS official"), key);
        assert_eq!(artist_key("Björk"), "björk");
    }

    #[test]
    fn song_uploads_share_one_key() {
        let key = title_key("Bailando");
        assert_eq!(
            title_key("Enrique Iglesias - Bailando ft. Descemer Bueno, Gente De Zona"),
            key
        );
        assert_eq!(title_key("Bailando (Official Video)"), key);
        assert_eq!(title_key("BAILANDO [Letra] [4K]"), key);
        assert_eq!(title_key("Enrique Iglesias – Bailando"), key);
        assert_ne!(title_key("Bailando (Live at Madison Square Garden)"), key);
        assert_eq!(title_key("Bailando (Live)"), "bailandolive");
        assert_eq!(title_key("Song (Official Video"), "songofficialvideo");
        assert_eq!(title_key("Song [Lyrics] (Remix)"), "songremix");
    }

    #[test]
    fn display_forms_keep_letters_and_split_the_performer() {
        assert_eq!(
            song_title("Enrique Iglesias - Bailando ft. Descemer Bueno (Official Video)"),
            SongTitle {
                performer: Some("Enrique Iglesias".into()),
                song: "Bailando".into()
            }
        );
        assert_eq!(
            song_title("Tití Me Preguntó  [Visualizer]"),
            SongTitle {
                performer: None,
                song: "Tití Me Preguntó".into()
            }
        );
        assert_eq!(song_title("Song (Remix) FEAT. Other").song, "Song Remix");
        assert_eq!(song_title(" - Untitled").performer, None);
        assert_eq!(clean_artist("Bad Bunny - Topic"), "Bad Bunny");
        assert_eq!(clean_artist("ShakiraVEVO"), "Shakira");
        assert_eq!(clean_artist("İstanbul Official"), "İstanbul");
    }
}
