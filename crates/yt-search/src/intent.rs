//! The «top result» of a search: the one item that best answers the query.

use crate::fuzzy::{word_budget, EditScratch};
use crate::normalize::{ends_in_word, fold, split_folded, tokens};

/// Kind of item chosen as the top result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TopKind {
    /// An artist whose name is the query, give or take typos.
    Artist,
    /// A song whose title contains every query word.
    Song,
    /// An album whose title contains every query word.
    Album,
    /// The best hit of the library search.
    Library,
}

/// The top result for `query`: the kind and its index in the matching slice
/// (`artist_names`, `song_titles` or `album_titles`; always 0 for
/// [`TopKind::Library`], the best library hit). The first rule that holds
/// wins:
///
/// 1. **Artist**: an artist whose name, folded and with spaces and
///    punctuation removed («Peso Pluma» → `pesopluma`), is within the
///    [`typo_budget`](crate::typo_budget) of the query taken the same way
///    (none for a number). Fewer edits win, then the earlier artist.
/// 2. **Song**: a title in which every query word matches a different title
///    word, exactly, within that word's typo budget, or, for the last word
///    while it is still being typed (nothing follows it in `query`), as the
///    start of a title word. Fewer typos win, then titles where the words
///    appear together and in order, then the earlier title.
/// 3. **Album**: the same rule over `album_titles`.
/// 4. **Library**: `library_best`, the score of the best library hit
///    ([`Hit::score`](crate::Hit::score)), when it is positive and finite.
///
/// `None` for a query without tokens or when no rule holds. Callers pass
/// entities in their own order of preference (provider or library order).
pub fn top_result(
    query: &str,
    artist_names: &[&str],
    song_titles: &[&str],
    album_titles: &[&str],
    library_best: Option<f64>,
) -> Option<(TopKind, usize)> {
    let folded = fold(query);
    let words: Vec<Vec<char>> = split_folded(&folded)
        .iter()
        .map(|word| word.chars().collect())
        .collect();
    if words.is_empty() {
        return None;
    }
    let mut scratch = EditScratch::default();
    if let Some(index) = closest_artist(&words, artist_names, &mut scratch) {
        return Some((TopKind::Artist, index));
    }
    let open = ends_in_word(query);
    if let Some(index) = best_title(&words, open, song_titles, &mut scratch) {
        return Some((TopKind::Song, index));
    }
    if let Some(index) = best_title(&words, open, album_titles, &mut scratch) {
        return Some((TopKind::Album, index));
    }
    library_best
        .filter(|score| score.is_finite() && *score > 0.0)
        .map(|_| (TopKind::Library, 0))
}

fn closest_artist(words: &[Vec<char>], names: &[&str], scratch: &mut EditScratch) -> Option<usize> {
    let query = words.concat();
    let budget = word_budget(&query);
    names
        .iter()
        .enumerate()
        .filter_map(|(index, name)| {
            let name: Vec<char> = tokens(name).concat().chars().collect();
            if name.is_empty() || name.len().abs_diff(query.len()) > budget {
                return None;
            }
            scratch
                .distances(&query, &name, budget)
                .whole
                .map(|distance| (distance, index))
        })
        .min()
        .map(|(_, index)| index)
}

fn best_title(
    words: &[Vec<char>],
    open: bool,
    titles: &[&str],
    scratch: &mut EditScratch,
) -> Option<usize> {
    titles
        .iter()
        .enumerate()
        .filter_map(|(index, title)| {
            title_match(words, open, title, scratch)
                .map(|found| (found.typos, !found.together, index))
        })
        .min()
        .map(|(_, _, index)| index)
}

/// How a title matched every query word.
struct TitleMatch {
    /// Edits summed over the query words.
    typos: usize,
    /// The matched title words are consecutive and in query order.
    together: bool,
}

/// Matches each query word, in order, to the unused title word it fits best;
/// `None` when some word fits none.
fn title_match(
    words: &[Vec<char>],
    open: bool,
    title: &str,
    scratch: &mut EditScratch,
) -> Option<TitleMatch> {
    let title: Vec<Vec<char>> = tokens(title)
        .iter()
        .map(|word| word.chars().collect())
        .collect();
    let mut used = vec![false; title.len()];
    let mut positions = Vec::with_capacity(words.len());
    let mut typos = 0;
    for (position, word) in words.iter().enumerate() {
        let completes = open && position + 1 == words.len();
        let budget = word_budget(word);
        let (cost, slot) = title
            .iter()
            .enumerate()
            .filter(|(slot, _)| !used[*slot])
            .filter_map(|(slot, candidate)| {
                word_cost(word, candidate, completes, budget, scratch).map(|cost| (cost, slot))
            })
            .min()?;
        used[slot] = true;
        positions.push(slot);
        typos += cost;
    }
    Some(TitleMatch {
        typos,
        together: positions.windows(2).all(|pair| pair[1] == pair[0] + 1),
    })
}

/// Edits for `word` to match `candidate`: 0 when equal or, with `completes`,
/// when `word` starts `candidate`; otherwise the distance within `budget`.
fn word_cost(
    word: &[char],
    candidate: &[char],
    completes: bool,
    budget: usize,
    scratch: &mut EditScratch,
) -> Option<usize> {
    if word == candidate || (completes && candidate.starts_with(word)) {
        return Some(0);
    }
    if budget == 0 || word.len().abs_diff(candidate.len()) > budget {
        return None;
    }
    scratch.distances(word, candidate, budget).whole
}

#[cfg(test)]
mod tests {
    use super::*;

    const SONGS: [&str; 3] = [
        "Peso Pluma - Ella Baila Sola (Official Video)",
        "Bailando",
        "La Bamba",
    ];

    #[test]
    fn artist_named_like_the_query_wins() {
        let artists = ["Natanael Cano", "Peso Pluma"];
        assert_eq!(
            top_result("peso pluma", &artists, &SONGS, &[], Some(3.0)),
            Some((TopKind::Artist, 1))
        );
        assert_eq!(
            top_result("PESO  PLUMA", &artists, &SONGS, &[], None),
            Some((TopKind::Artist, 1))
        );
        assert_eq!(
            top_result("pesso pluma", &artists, &SONGS, &[], None),
            Some((TopKind::Artist, 1))
        );
        assert_eq!(
            top_result("pesopluma", &artists, &SONGS, &[], None),
            Some((TopKind::Artist, 1))
        );
        // A partial name is not the artist.
        assert_ne!(
            top_result("peso", &artists, &SONGS, &[], None),
            Some((TopKind::Artist, 1))
        );
    }

    #[test]
    fn song_wins_when_no_artist_matches() {
        let artists = ["Natanael Cano"];
        assert_eq!(
            top_result("peso pluma", &artists, &SONGS, &[], Some(3.0)),
            Some((TopKind::Song, 0))
        );
        assert_eq!(
            top_result("ella baila so", &artists, &SONGS, &[], None),
            Some((TopKind::Song, 0))
        );
        assert_eq!(
            top_result("bailnado", &artists, &SONGS, &[], None),
            Some((TopKind::Song, 1))
        );
        assert_eq!(
            top_result("bamba la", &artists, &SONGS, &[], None),
            Some((TopKind::Song, 2))
        );
    }

    #[test]
    fn songs_without_typos_and_with_the_words_together_come_first() {
        let songs = ["Bailado", "Bailando"];
        assert_eq!(
            top_result("bailando", &[], &songs, &[], None),
            Some((TopKind::Song, 1))
        );
        let songs = ["Sola, baila ella", "Ella baila sola"];
        assert_eq!(
            top_result("ella baila sola", &[], &songs, &[], None),
            Some((TopKind::Song, 1))
        );
        let songs = ["Bailando", "Bailando (Remix)"];
        assert_eq!(
            top_result("bailando", &[], &songs, &[], None),
            Some((TopKind::Song, 0))
        );
    }

    #[test]
    fn album_then_library_then_nothing() {
        let albums = ["Grandes Éxitos", "Sex and Love"];
        assert_eq!(
            top_result("sex love", &[], &SONGS, &albums, Some(2.0)),
            Some((TopKind::Album, 1))
        );
        assert_eq!(
            top_result("exitos", &[], &SONGS, &albums, None),
            Some((TopKind::Album, 0))
        );
        assert_eq!(
            top_result("hero", &[], &SONGS, &albums, Some(2.5)),
            Some((TopKind::Library, 0))
        );
        for best in [
            None,
            Some(0.0),
            Some(-1.0),
            Some(f64::NAN),
            Some(f64::INFINITY),
        ] {
            assert_eq!(
                top_result("hero", &[], &SONGS, &albums, best),
                None,
                "{best:?}"
            );
        }
        assert_eq!(
            top_result("   ", &["Peso Pluma"], &SONGS, &albums, Some(2.0)),
            None
        );
    }

    #[test]
    fn every_query_word_must_match_a_distinct_title_word() {
        assert_eq!(
            top_result("bailando remix", &[], &["Bailando"], &[], None),
            None
        );
        assert_eq!(top_result("la la", &[], &["La Bamba"], &[], None), None);
        assert_eq!(
            top_result("la la", &[], &["La La Land"], &[], None),
            Some((TopKind::Song, 0))
        );
        // Numbers are never corrected.
        assert_eq!(
            top_result("2023", &["2024"], &["Hits 2024"], &[], None),
            None
        );
    }
}
