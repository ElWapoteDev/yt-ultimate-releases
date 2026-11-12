//! Typo tolerance: a bounded optimal string alignment distance, typo budgets
//! by token length, character-class filters and trigram similarity.

/// Shortest token, in characters, that tolerates one typo.
pub const ONE_TYPO_MIN_CHARS: usize = 4;
/// Shortest token, in characters, that tolerates two typos.
pub const TWO_TYPOS_MIN_CHARS: usize = 8;
/// Padding around a word when cutting trigrams, so its first and last
/// letters take part in as many trigrams as the inner ones.
const TRIGRAM_PAD: char = ' ';
/// Bits of a character mask owned by `a`–`z`.
const LETTER_BITS: u32 = 26;
/// Bits of a character mask owned by `0`–`9`.
const DIGIT_BITS: u32 = 10;
/// Bits every other character hashes into.
const SHARED_BITS: u32 = u64::BITS - LETTER_BITS - DIGIT_BITS;

/// Typos tolerated in a token of `token_len` characters: none below
/// [`ONE_TYPO_MIN_CHARS`] (short words have too many neighbors for a typo to
/// be recoverable), one below [`TWO_TYPOS_MIN_CHARS`], two from there on.
pub fn typo_budget(token_len: usize) -> usize {
    if token_len >= TWO_TYPOS_MIN_CHARS {
        2
    } else if token_len >= ONE_TYPO_MIN_CHARS {
        1
    } else {
        0
    }
}

/// Typo budget of a folded token; numbers get none, because a near number
/// (a year, a track number) names a different thing.
pub(crate) fn word_budget(word: &[char]) -> usize {
    if word.iter().all(|character| character.is_numeric()) {
        0
    } else {
        typo_budget(word.len())
    }
}

/// Optimal string alignment distance between `a` and `b` (Damerau–Levenshtein
/// where insertions, deletions, substitutions and transpositions of adjacent
/// characters cost one edit and no substring is edited twice), counted in
/// Unicode scalar values, when it is at most `max`; `None` otherwise.
///
/// Characters are compared as given: fold both sides first when case and
/// accents must not count. Work stops as early as the bound allows: lengths
/// that differ by more than `max` are rejected before any work, only the
/// diagonal band of width `2·max + 1` is computed, and the computation ends
/// at the first row from which every alignment already exceeds `max`. Time
/// O(len · (2·max + 1)), memory O(len).
pub fn edit_distance_within(a: &str, b: &str, max: usize) -> Option<usize> {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    EditScratch::default().distances(&a, &b, max).whole
}

/// Similarity of `a` and `b` as the Jaccard index of their sets of character
/// trigrams, each word padded with two spaces before and one after: 1 for
/// equal strings (two empty strings included), 0 when no trigram is shared
/// (an empty string against a non-empty one). Symmetric. Characters are
/// compared as given; fold first to ignore case and accents.
///
/// Used to order typo corrections of equal distance: a correction that keeps
/// the typed word's start and its runs of letters ranks first.
pub fn trigram_similarity(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    trigram_similarity_chars(&a, &b)
}

/// [`trigram_similarity`] over character slices.
pub(crate) fn trigram_similarity_chars(a: &[char], b: &[char]) -> f64 {
    if a == b {
        return 1.0;
    }
    let left = trigrams(a);
    let right = trigrams(b);
    let shared = shared_count(&left, &right);
    let union = left.len() + right.len() - shared;
    shared as f64 / union as f64
}

fn trigrams(word: &[char]) -> Vec<[char; 3]> {
    if word.is_empty() {
        return Vec::new();
    }
    let padded: Vec<char> = [TRIGRAM_PAD, TRIGRAM_PAD]
        .into_iter()
        .chain(word.iter().copied())
        .chain([TRIGRAM_PAD])
        .collect();
    let mut grams: Vec<[char; 3]> = padded
        .windows(3)
        .map(|window| [window[0], window[1], window[2]])
        .collect();
    grams.sort_unstable();
    grams.dedup();
    grams
}

/// Elements present in both sorted, deduplicated slices.
fn shared_count(left: &[[char; 3]], right: &[[char; 3]]) -> usize {
    let (mut index_left, mut index_right, mut shared) = (0, 0, 0);
    while let (Some(gram_left), Some(gram_right)) = (left.get(index_left), right.get(index_right)) {
        match gram_left.cmp(gram_right) {
            std::cmp::Ordering::Less => index_left += 1,
            std::cmp::Ordering::Greater => index_right += 1,
            std::cmp::Ordering::Equal => {
                shared += 1;
                index_left += 1;
                index_right += 1;
            }
        }
    }
    shared
}

/// Set of character classes in `word`: `a`–`z` and `0`–`9` own a bit each,
/// other characters share hashed bits. When one word has `n` classes the
/// other lacks, they are at least `n` edits apart, which rejects most of a
/// vocabulary before the dynamic program runs.
pub(crate) fn char_mask(word: &[char]) -> u64 {
    word.iter()
        .fold(0, |mask, &character| mask | 1 << char_bit(character))
}

/// Edits needed at least to remove from a word with mask `from` every class
/// that a word with mask `to` lacks: each such class needs its own deletion
/// or substitution. Also a lower bound against any prefix of the second word.
pub(crate) fn missing_classes(from: u64, to: u64) -> usize {
    (from & !to).count_ones() as usize
}

fn char_bit(character: char) -> u32 {
    match character {
        'a'..='z' => character as u32 - 'a' as u32,
        '0'..='9' => LETTER_BITS + (character as u32 - '0' as u32),
        _ => LETTER_BITS + DIGIT_BITS + character as u32 % SHARED_BITS,
    }
}

/// Distances one banded pass finds between a query word and a term.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Distances {
    /// Distance to the whole term, when within the bound.
    pub(crate) whole: Option<usize>,
    /// Smallest distance to any prefix of the term (the empty prefix and the
    /// whole term included), when within the bound.
    pub(crate) prefix: Option<usize>,
}

impl Distances {
    const NONE: Self = Self {
        whole: None,
        prefix: None,
    };
}

/// Rows of the banded dynamic program, kept between calls so scanning a
/// vocabulary allocates once.
#[derive(Debug, Default)]
pub(crate) struct EditScratch {
    two_back: Vec<usize>,
    previous: Vec<usize>,
    current: Vec<usize>,
    /// Query rows the last call computed, so tests can observe the early exit.
    #[cfg(test)]
    rows_computed: usize,
}

impl EditScratch {
    /// Optimal string alignment distances from `query` to `term` and to the
    /// term's prefixes, each reported only when at most `max` (Ukkonen's
    /// cut-off: cells farther than `max` from the diagonal are never filled).
    pub(crate) fn distances(&mut self, query: &[char], term: &[char], max: usize) -> Distances {
        #[cfg(test)]
        {
            self.rows_computed = 0;
        }
        let rows = query.len();
        // No distance exceeds the longer length, so a larger bound changes nothing.
        let max = max.min(rows.max(term.len()));
        if term.len() + max < rows {
            return Distances::NONE;
        }
        if rows == 0 || term.is_empty() {
            let whole = Some(rows.max(term.len())).filter(|distance| *distance <= max);
            let prefix = if rows == 0 { Some(0) } else { whole };
            return Distances { whole, prefix };
        }
        // Columns past `rows + max` cannot reach the last row inside the band.
        let columns = term.len().min(rows + max);
        let infinity = max + 1;
        for row in [&mut self.two_back, &mut self.previous, &mut self.current] {
            row.clear();
            row.resize(columns + 1, infinity);
        }
        for (column, cell) in self.previous.iter_mut().enumerate().take(max + 1) {
            *cell = column;
        }
        let mut previous_min = 0;
        for row in 1..=rows {
            #[cfg(test)]
            {
                self.rows_computed = row;
            }
            let low = row.saturating_sub(max).max(1);
            let high = (row + max).min(columns);
            self.current[0] = if row <= max { row } else { infinity };
            if low > 1 {
                self.current[low - 1] = infinity;
            }
            if high < columns {
                self.current[high + 1] = infinity;
            }
            let mut row_min = self.current[0];
            for column in low..=high {
                let replace =
                    self.previous[column - 1] + usize::from(query[row - 1] != term[column - 1]);
                let mut best = replace
                    .min(self.previous[column] + 1)
                    .min(self.current[column - 1] + 1);
                if row > 1
                    && column > 1
                    && query[row - 1] == term[column - 2]
                    && query[row - 2] == term[column - 1]
                {
                    best = best.min(self.two_back[column - 2] + 1);
                }
                let best = best.min(infinity);
                self.current[column] = best;
                row_min = row_min.min(best);
            }
            // Later rows grow from this row, or from the previous one through a
            // transposition that costs one more edit.
            if row_min > max && previous_min >= max {
                return Distances::NONE;
            }
            std::mem::swap(&mut self.two_back, &mut self.previous);
            std::mem::swap(&mut self.previous, &mut self.current);
            previous_min = row_min;
        }
        let last = &self.previous;
        let whole = (columns == term.len())
            .then(|| last[columns])
            .filter(|distance| *distance <= max);
        let prefix = last[rows.saturating_sub(max)..=columns]
            .iter()
            .copied()
            .min()
            .filter(|distance| *distance <= max);
        Distances { whole, prefix }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    /// Unbounded optimal string alignment distance, the reference the
    /// bounded version must agree with.
    fn reference(a: &str, b: &str) -> usize {
        let (a, b) = (chars(a), chars(b));
        let mut table = vec![vec![0; b.len() + 1]; a.len() + 1];
        for (row, cells) in table.iter_mut().enumerate() {
            cells[0] = row;
        }
        for (column, cell) in table[0].iter_mut().enumerate() {
            *cell = column;
        }
        for row in 1..=a.len() {
            for column in 1..=b.len() {
                let cost = usize::from(a[row - 1] != b[column - 1]);
                let mut best = (table[row - 1][column] + 1)
                    .min(table[row][column - 1] + 1)
                    .min(table[row - 1][column - 1] + cost);
                if row > 1
                    && column > 1
                    && a[row - 1] == b[column - 2]
                    && a[row - 2] == b[column - 1]
                {
                    best = best.min(table[row - 2][column - 2] + 1);
                }
                table[row][column] = best;
            }
        }
        table[a.len()][b.len()]
    }

    #[test]
    fn distance_counts_every_kind_of_edit() {
        assert_eq!(edit_distance_within("bailando", "bailando", 0), Some(0));
        assert_eq!(edit_distance_within("bailnado", "bailando", 2), Some(1));
        assert_eq!(edit_distance_within("enrrique", "enrique", 2), Some(1));
        assert_eq!(edit_distance_within("kitten", "sitting", 3), Some(3));
        assert_eq!(edit_distance_within("ab", "ba", 1), Some(1));
        assert_eq!(edit_distance_within("", "abc", 3), Some(3));
        assert_eq!(edit_distance_within("abc", "", 5), Some(3));
        assert_eq!(edit_distance_within("", "", 0), Some(0));
        assert_eq!(edit_distance_within("ñu", "nu", 1), Some(1));
        // Optimal string alignment never edits a transposed pair again.
        assert_eq!(edit_distance_within("ca", "abc", 3), Some(3));
    }

    #[test]
    fn distance_respects_the_bound_and_matches_the_reference() {
        assert_eq!(edit_distance_within("kitten", "sitting", 2), None);
        assert_eq!(edit_distance_within("abc", "abcdef", 2), None);
        assert_eq!(edit_distance_within("abcdef", "uvwxyz", 1), None);
        assert_eq!(edit_distance_within("abc", "xyz", usize::MAX), Some(3));
        let words = [
            "",
            "a",
            "ab",
            "ba",
            "abc",
            "acb",
            "bailando",
            "bailnado",
            "bailado",
            "ballad",
            "enrique",
            "enrrique",
            "eniqrue",
            "canción",
            "cancion",
            "peso pluma",
            "pesopluma",
        ];
        let mut scratch = EditScratch::default();
        for left in words {
            for right in words {
                let distance = reference(left, right);
                let prefixes: Vec<char> = chars(right);
                let prefix_distance = (0..=prefixes.len())
                    .map(|end| reference(left, &prefixes[..end].iter().collect::<String>()))
                    .min()
                    .unwrap_or(distance);
                for max in 0..=distance + 1 {
                    let expected = (distance <= max).then_some(distance);
                    assert_eq!(
                        edit_distance_within(left, right, max),
                        expected,
                        "{left} {right} {max}"
                    );
                    let expected = (prefix_distance <= max).then_some(prefix_distance);
                    let found = scratch.distances(&chars(left), &prefixes, max).prefix;
                    assert_eq!(found, expected, "prefix {left} {right} {max}");
                }
            }
        }
    }

    #[test]
    fn distance_stops_at_the_first_row_beyond_the_bound() {
        let query = chars(&"a".repeat(1_000));
        let term = chars(&"b".repeat(1_000));
        let mut scratch = EditScratch::default();
        assert_eq!(scratch.distances(&query, &term, 2), Distances::NONE);
        assert!(scratch.rows_computed <= 4, "{} rows", scratch.rows_computed);
        // A term too short for any alignment within the bound needs no rows.
        assert_eq!(scratch.distances(&query, &term[..990], 2), Distances::NONE);
        assert_eq!(scratch.rows_computed, 0);
    }

    #[test]
    fn prefix_distance_finds_words_still_being_typed() {
        let mut scratch = EditScratch::default();
        let distances = scratch.distances(&chars("bial"), &chars("bailando"), 1);
        assert_eq!(
            distances,
            Distances {
                whole: None,
                prefix: Some(1)
            }
        );
        let distances = scratch.distances(&chars("baila"), &chars("bailando"), 1);
        assert_eq!(
            distances,
            Distances {
                whole: None,
                prefix: Some(0)
            }
        );
        let distances = scratch.distances(&chars("bailando"), &chars("bailando"), 2);
        assert_eq!(
            distances,
            Distances {
                whole: Some(0),
                prefix: Some(0)
            }
        );
        let distances = scratch.distances(&chars("zzzz"), &chars("bailando"), 1);
        assert_eq!(distances, Distances::NONE);
    }

    #[test]
    fn typo_budget_grows_with_token_length() {
        for (length, budget) in [(0, 0), (1, 0), (3, 0), (4, 1), (7, 1), (8, 2), (30, 2)] {
            assert_eq!(typo_budget(length), budget, "{length}");
        }
        assert_eq!(word_budget(&chars("2023")), 0);
        assert_eq!(word_budget(&chars("abba")), 1);
    }

    #[test]
    fn trigram_similarity_rewards_shared_runs_of_letters() {
        assert_eq!(trigram_similarity("bailando", "bailando"), 1.0);
        assert_eq!(trigram_similarity("", ""), 1.0);
        assert_eq!(trigram_similarity("", "abc"), 0.0);
        assert_eq!(trigram_similarity("abc", "xyz"), 0.0);
        let close = trigram_similarity("bailando", "bailamos");
        assert!(close > trigram_similarity("bailando", "cantando"));
        assert!((close - trigram_similarity("bailamos", "bailando")).abs() < f64::EPSILON);
        assert!(close > 0.0 && close < 1.0);
    }

    #[test]
    fn character_classes_bound_the_distance_from_below() {
        let words = [
            "bailando", "bailnado", "ballad", "enrique", "x1y2", "ñandú", "a",
        ];
        for left in words {
            for right in words {
                let (a, b) = (chars(left), chars(right));
                let bound = missing_classes(char_mask(&a), char_mask(&b))
                    .max(missing_classes(char_mask(&b), char_mask(&a)));
                assert!(bound <= reference(left, right), "{left} {right}");
            }
        }
    }
}
