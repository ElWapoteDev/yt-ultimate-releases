//! Text folding and tokenization shared by every matcher in this crate.
//!
//! Folding makes text comparable the way people type it: case, accents,
//! compatibility forms (full-width letters, ligatures) and spacing stop
//! mattering, while letters, digits and punctuation stay.

use unicode_normalization::char::is_combining_mark;
pub use yt_core::fold;
pub(crate) use yt_core::fold_into;
use yt_core::FOLDED_APOSTROPHE;

/// Search tokens of `text`: its [`fold`] split at every character that is
/// not a letter or a digit, without empty tokens. An apostrophe between two
/// letters or digits joins them instead («Don't» → `dont`), because people
/// type such words with and without it. Total for any input.
pub fn tokens(text: &str) -> Vec<String> {
    split_folded(&fold(text))
}

/// [`tokens`] of text that is already folded.
pub(crate) fn split_folded(folded: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut characters = folded.chars().peekable();
    while let Some(character) = characters.next() {
        if character.is_alphanumeric() {
            current.push(character);
            continue;
        }
        let joins = character == FOLDED_APOSTROPHE
            && !current.is_empty()
            && characters.peek().is_some_and(|next| next.is_alphanumeric());
        if !joins && !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Whether raw `text` ends inside its last token (its last character, past
/// any trailing combining marks, is a letter or a digit), so the person may
/// still be typing that word. Checked before folding, which trims the space
/// that marks a finished word.
pub(crate) fn ends_in_word(text: &str) -> bool {
    text.chars()
        .rev()
        .find(|character| !is_combining_mark(*character))
        .is_some_and(char::is_alphanumeric)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_split_on_everything_but_letters_and_digits() {
        assert_eq!(
            tokens("AC/DC - Back In Black (Live, 1980)"),
            ["ac", "dc", "back", "in", "black", "live", "1980"]
        );
        assert_eq!(tokens("Don’t Stop Me Now"), ["dont", "stop", "me", "now"]);
        assert_eq!(
            tokens("'quoted' rock 'n' roll"),
            ["quoted", "rock", "n", "roll"]
        );
        assert_eq!(tokens("Canción #1 — Señor"), ["cancion", "1", "senor"]);
        assert!(tokens("").is_empty());
        assert!(tokens(" -- !! ").is_empty());
    }

    #[test]
    fn open_last_word_is_detected_before_folding() {
        assert!(ends_in_word("bad bun"));
        assert!(ends_in_word("cancio\u{301}"));
        assert!(ends_in_word("ＡＢ"));
        assert!(!ends_in_word("bad "));
        assert!(!ends_in_word("bad\u{a0}"));
        assert!(!ends_in_word("bad,"));
        assert!(!ends_in_word(""));
    }
}
