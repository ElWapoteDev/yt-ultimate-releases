//! BM25F relevance over an in-memory inverted index, with as-you-type
//! completion of the last query token and typo tolerance for tokens the
//! vocabulary does not know.
//!
//! Scoring is the simple BM25F of Robertson & Zaragoza (2009): each field's
//! term frequency is normalized by that field's length, weighted, summed into
//! one pseudo-frequency and saturated once, so a term repeated across fields
//! never counts linearly:
//!
//! ```text
//! tf'(t, d)  = Σ_f  w_f · tf(t, d, f) / ((1 − b) + b · len(d, f) / avglen(f))
//! score(q, d) = Σ_q  max over the terms t that q matches of
//!                    penalty(q, t) · idf(t) · tf'(t, d) / (k1 + tf'(t, d))
//! idf(t)     = ln(1 + (N − df(t) + 0.5) / (df(t) + 0.5))
//! ```

use crate::fuzzy::{
    char_mask, missing_classes, trigram_similarity_chars, word_budget, EditScratch,
};
use crate::normalize::{ends_in_word, fold, split_folded, tokens};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::ops::Range;

/// Number of [`Field`] variants.
const FIELD_COUNT: usize = Field::ALL.len();

// Query tokens are tracked as bits of a `u32`.
const _: () = assert!(Index::MAX_QUERY_TOKENS < u32::BITS as usize);

/// Part of a document a text belongs to. Fields differ only by their weight
/// in [`Bm25Config`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Field {
    /// Song or video title.
    Title,
    /// Performing artist or channel.
    Artist,
    /// Album or collection.
    Album,
    /// Anything else worth finding, such as a file name or tags.
    Other,
}

impl Field {
    /// Every field, in the order [`Hit::matched`] reports them.
    pub const ALL: [Self; 4] = [Self::Title, Self::Artist, Self::Album, Self::Other];

    fn index(self) -> usize {
        self as usize
    }

    fn bit(self) -> u8 {
        1 << self.index()
    }
}

/// One searchable item, such as a library track or an online result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// Identifier reported in [`Hit::id`]. When several documents share an
    /// id, only the first one is indexed.
    pub id: String,
    /// Texts by field. A field may appear more than once; its values count
    /// together for its length and separately for phrases.
    pub fields: Vec<(Field, String)>,
}

/// Parameters of BM25F scoring and of the as-you-type and typo matches.
///
/// [`Index::build`] clamps every value into its documented range and
/// replaces a non-finite value with its default.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bm25Config {
    /// Term-frequency saturation, at least 0: 0 counts presence only, larger
    /// values let repetitions count longer.
    pub k1: f64,
    /// Length normalization in [0, 1]: 0 ignores field length, 1 divides by
    /// the length relative to the field's average.
    pub b: f64,
    /// Weight of [`Field::Title`], at least 0. A field of weight 0 is not
    /// searched.
    pub title_weight: f64,
    /// Weight of [`Field::Artist`], at least 0.
    pub artist_weight: f64,
    /// Weight of [`Field::Album`], at least 0.
    pub album_weight: f64,
    /// Weight of [`Field::Other`], at least 0.
    pub other_weight: f64,
    /// Share of a match removed when the last query token only starts a
    /// longer term, in [0, 1]; 1 turns completions off.
    pub prefix_penalty: f64,
    /// Share of a match removed per edit of a typo correction, in [0, 1]
    /// (one edit keeps `1 − typo_penalty`, two keep its square); 1 turns
    /// corrections off.
    pub typo_penalty: f64,
    /// Relative bonus, at least 0, for a document where the query tokens
    /// appear together and in order in one field value; scaled by that
    /// field's weight relative to the strongest field.
    pub phrase_bonus: f64,
}

impl Default for Bm25Config {
    /// `k1` 1.2 and `b` 0.75 (the usual BM25 starting point); fields weigh
    /// title 3.0, artist 2.0, album 1.2 and other 0.6; a completion keeps 75 %
    /// of a match, a typo 60 % per edit; a phrase adds up to 50 %.
    fn default() -> Self {
        Self {
            k1: 1.2,
            b: 0.75,
            title_weight: 3.0,
            artist_weight: 2.0,
            album_weight: 1.2,
            other_weight: 0.6,
            prefix_penalty: 0.25,
            typo_penalty: 0.4,
            phrase_bonus: 0.5,
        }
    }
}

impl Bm25Config {
    /// Weight of `field`.
    pub fn weight(&self, field: Field) -> f64 {
        match field {
            Field::Title => self.title_weight,
            Field::Artist => self.artist_weight,
            Field::Album => self.album_weight,
            Field::Other => self.other_weight,
        }
    }

    fn strongest_weight(&self) -> f64 {
        Field::ALL
            .into_iter()
            .map(|field| self.weight(field))
            .fold(0.0, f64::max)
    }

    fn sanitized(self) -> Self {
        let fallback = Self::default();
        let at_least_zero = |value: f64, default: f64| {
            if value.is_finite() {
                value.max(0.0)
            } else {
                default
            }
        };
        let unit = |value: f64, default: f64| {
            if value.is_finite() {
                value.clamp(0.0, 1.0)
            } else {
                default
            }
        };
        Self {
            k1: at_least_zero(self.k1, fallback.k1),
            b: unit(self.b, fallback.b),
            title_weight: at_least_zero(self.title_weight, fallback.title_weight),
            artist_weight: at_least_zero(self.artist_weight, fallback.artist_weight),
            album_weight: at_least_zero(self.album_weight, fallback.album_weight),
            other_weight: at_least_zero(self.other_weight, fallback.other_weight),
            prefix_penalty: unit(self.prefix_penalty, fallback.prefix_penalty),
            typo_penalty: unit(self.typo_penalty, fallback.typo_penalty),
            phrase_bonus: at_least_zero(self.phrase_bonus, fallback.phrase_bonus),
        }
    }
}

/// One search result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// Id of the matching document.
    pub id: String,
    /// Relevance, positive; higher is better. Comparable only between hits
    /// of one search.
    pub score: f64,
    /// Fields where a query token matched, in [`Field::ALL`] order.
    pub matched: Vec<Field>,
    /// Every query token matched a term of this document exactly, without a
    /// completion or a typo correction.
    pub exact: bool,
}

/// An immutable index over a set of documents, built once and searched many
/// times. Searching takes `&self`, so one index can serve several threads.
#[derive(Clone)]
pub struct Index {
    config: Bm25Config,
    ids: Vec<String>,
    /// `spans[span_starts[d]..span_starts[d + 1]]` are document `d`'s values.
    span_starts: Vec<usize>,
    spans: Vec<Span>,
    /// Term ids of every indexed value, in order, for phrase checks.
    sequence: Vec<usize>,
    vocabulary: Vocabulary,
    postings: Postings,
}

/// One field value of a document, as a run of `Index::sequence`.
#[derive(Debug, Clone, Copy)]
struct Span {
    field: Field,
    start: usize,
    end: usize,
}

/// One document containing a term.
#[derive(Debug, Clone, Copy)]
struct Posting {
    doc: usize,
    /// `tf' / (k1 + tf')` of the term in this document.
    saturation: f64,
    /// Bits of the fields that contain the term.
    fields: u8,
}

/// Distinct terms in code-point order, so a prefix selects a contiguous run.
#[derive(Debug, Clone, Default)]
struct Vocabulary {
    terms: Vec<Box<[char]>>,
    masks: Vec<u64>,
}

/// Postings of every term, grouped by term and ordered by document.
#[derive(Debug, Clone, Default)]
struct Postings {
    starts: Vec<usize>,
    entries: Vec<Posting>,
}

/// Vocabulary terms one query token matches, sorted by term.
struct Expansion {
    matches: Vec<TermMatch>,
}

#[derive(Debug, Clone, Copy)]
struct TermMatch {
    term: usize,
    /// Share of the term's score the token earns: 1 for an exact match.
    weight: f64,
    exact: bool,
}

/// A typo correction considered for a query token.
struct Correction {
    term: usize,
    distance: usize,
    weight: f64,
}

/// Per-document accumulators of one search.
struct Tally {
    score: Vec<f64>,
    /// Bit `i` set when query token `i` matched the document.
    covered: Vec<u32>,
    /// Bit `i` set when query token `i` matched the document exactly.
    exact: Vec<u32>,
    fields: Vec<u8>,
}

/// A candidate's sort key: more query tokens first, then score, then the
/// order documents were given in.
#[derive(Debug, Clone, Copy)]
struct Ranked {
    matched_tokens: u32,
    score: f64,
    doc: usize,
}

impl Index {
    /// Most query tokens a search considers; later tokens are ignored.
    pub const MAX_QUERY_TOKENS: usize = 16;
    /// Most typo corrections tried for one query token.
    pub const MAX_CORRECTIONS: usize = 128;

    /// Indexes `docs` with `config` (sanitized as [`Bm25Config`] documents).
    ///
    /// Every field value is [`tokens`]-ed; a document whose id was already
    /// seen is skipped. Average field lengths are taken over the documents
    /// that have the field. Time O(total tokens · log), memory O(total
    /// tokens + distinct terms).
    pub fn build(docs: &[Document], config: Bm25Config) -> Self {
        let config = config.sanitized();
        let mut seen = HashSet::new();
        let mut interned: HashMap<String, usize> = HashMap::new();
        let mut texts: Vec<String> = Vec::new();
        let mut ids = Vec::new();
        let mut span_starts = vec![0];
        let mut spans = Vec::new();
        let mut sequence = Vec::new();
        for doc in docs {
            if !seen.insert(doc.id.as_str()) {
                continue;
            }
            for (field, text) in &doc.fields {
                if config.weight(*field) <= 0.0 {
                    continue;
                }
                let start = sequence.len();
                for token in tokens(text) {
                    let next = texts.len();
                    let term = *interned.entry(token).or_insert_with_key(|token| {
                        texts.push(token.clone());
                        next
                    });
                    sequence.push(term);
                }
                if sequence.len() > start {
                    spans.push(Span {
                        field: *field,
                        start,
                        end: sequence.len(),
                    });
                }
            }
            ids.push(doc.id.clone());
            span_starts.push(spans.len());
        }

        let vocabulary = Vocabulary::sorted(&texts, &mut sequence);
        let mut index = Self {
            config,
            ids,
            span_starts,
            spans,
            sequence,
            vocabulary,
            postings: Postings::default(),
        };
        index.postings = index.weigh_postings();
        index
    }

    /// Postings carrying each term's BM25F saturation in each document.
    fn weigh_postings(&self) -> Postings {
        let lengths: Vec<[usize; FIELD_COUNT]> = (0..self.ids.len())
            .map(|doc| {
                let mut length = [0; FIELD_COUNT];
                for span in self.spans_of(doc) {
                    length[span.field.index()] += span.end - span.start;
                }
                length
            })
            .collect();
        let average = average_lengths(&lengths);
        let config = &self.config;
        let mut pending: Vec<(usize, Posting)> = Vec::new();
        let mut occurrences: Vec<(usize, Field)> = Vec::new();
        for (doc, length) in lengths.iter().enumerate() {
            occurrences.clear();
            for span in self.spans_of(doc) {
                let terms = &self.sequence[span.start..span.end];
                occurrences.extend(terms.iter().map(|term| (*term, span.field)));
            }
            occurrences.sort_unstable();
            for group in occurrences.chunk_by(|left, right| left.0 == right.0) {
                let mut frequency = [0usize; FIELD_COUNT];
                let mut fields = 0;
                for (_, field) in group {
                    frequency[field.index()] += 1;
                    fields |= field.bit();
                }
                let pseudo: f64 = Field::ALL
                    .into_iter()
                    .filter(|field| frequency[field.index()] > 0)
                    .map(|field| {
                        let index = field.index();
                        let relative = length[index] as f64 / average[index];
                        config.weight(field) * frequency[index] as f64
                            / (1.0 - config.b + config.b * relative)
                    })
                    .sum();
                let posting = Posting {
                    doc,
                    saturation: pseudo / (config.k1 + pseudo),
                    fields,
                };
                // `chunk_by` never yields an empty group.
                pending.push((group[0].0, posting));
            }
        }
        // Stable, so every term's documents stay in index order.
        pending.sort_by_key(|(term, _)| *term);
        Postings::new(self.vocabulary.len(), pending)
    }

    fn spans_of(&self, doc: usize) -> &[Span] {
        &self.spans[self.span_starts[doc]..self.span_starts[doc + 1]]
    }

    /// The best documents for `query`, at most `limit`, best first.
    ///
    /// The query is [`tokens`]-ed (at most [`Index::MAX_QUERY_TOKENS`]). A
    /// token matches a vocabulary term exactly; the last token, when nothing
    /// follows it in `query`, also matches every longer term it starts
    /// (penalized by `prefix_penalty`). A token that neither equals nor (as
    /// the last token) starts any term is corrected instead: it matches the
    /// terms at the smallest edit distance within its [`typo_budget`]
    /// (numbers get none), and the last token also matches terms whose
    /// beginning is that close; each edit is penalized by `typo_penalty`.
    /// A token scores, per document, its best match.
    ///
    /// Documents must match every token (AND). When none does, the result
    /// falls back to the documents matching the most tokens, ordered by how
    /// many they match and then by score. With full matches, a document
    /// where the tokens appear together and in order in one field value is
    /// boosted by `phrase_bonus`. Ties keep the order documents were given
    /// to [`Index::build`]. An empty query (no tokens) or a `limit` of 0
    /// returns nothing.
    ///
    /// [`typo_budget`]: crate::typo_budget
    pub fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        let folded = fold(query);
        let mut words = split_folded(&folded);
        // The last token is still being typed only when it ends the query.
        let open = ends_in_word(query) && words.len() <= Self::MAX_QUERY_TOKENS;
        words.truncate(Self::MAX_QUERY_TOKENS);
        if words.is_empty() || limit == 0 {
            return Vec::new();
        }
        let mut scratch = EditScratch::default();
        let last = words.len() - 1;
        let expansions: Vec<Expansion> = words
            .iter()
            .enumerate()
            .map(|(position, word)| self.expand(word, open && position == last, &mut scratch))
            .collect();
        let mut tally = self.tally(&expansions);
        let everything = (1u32 << expansions.len()) - 1;
        let mut candidates: Vec<usize> = (0..self.ids.len())
            .filter(|doc| tally.covered[*doc] == everything)
            .collect();
        if candidates.is_empty() {
            candidates = (0..self.ids.len())
                .filter(|doc| tally.covered[*doc] != 0)
                .collect();
        } else if expansions.len() > 1 {
            for doc in &candidates {
                tally.score[*doc] *= self.phrase_factor(*doc, &expansions);
            }
        }
        self.rank(&tally, candidates, everything, limit)
    }

    fn expand(&self, word: &str, open: bool, scratch: &mut EditScratch) -> Expansion {
        let word: Vec<char> = word.chars().collect();
        let exact = self.vocabulary.find(&word);
        let completions = if open {
            self.vocabulary.completions(&word)
        } else {
            0..0
        };
        let mut matches: Vec<TermMatch> = exact
            .map(|term| TermMatch {
                term,
                weight: 1.0,
                exact: true,
            })
            .into_iter()
            .chain(completions.clone().map(|term| TermMatch {
                term,
                weight: 1.0 - self.config.prefix_penalty,
                exact: false,
            }))
            .collect();
        if exact.is_none() && completions.is_empty() {
            matches.extend(
                self.corrections(&word, open, scratch)
                    .into_iter()
                    .map(|correction| TermMatch {
                        term: correction.term,
                        weight: correction.weight,
                        exact: false,
                    }),
            );
        }
        matches.retain(|found| found.weight > 0.0);
        matches.sort_unstable_by_key(|found| found.term);
        Expansion { matches }
    }

    /// Terms at the smallest edit distance from `word` within its budget;
    /// with `open`, distances to term beginnings count too.
    fn corrections(&self, word: &[char], open: bool, scratch: &mut EditScratch) -> Vec<Correction> {
        let budget = word_budget(word);
        if budget == 0 {
            return Vec::new();
        }
        let mask = char_mask(word);
        let kept = 1.0 - self.config.typo_penalty;
        let completed = 1.0 - self.config.prefix_penalty;
        let mut found = Vec::new();
        for (term, (text, term_mask)) in self
            .vocabulary
            .terms
            .iter()
            .zip(&self.vocabulary.masks)
            .enumerate()
        {
            let whole_possible = text.len().abs_diff(word.len()) <= budget
                && missing_classes(*term_mask, mask) <= budget;
            if !(whole_possible || open) || missing_classes(mask, *term_mask) > budget {
                continue;
            }
            let distances = scratch.distances(word, text, budget);
            let whole = distances
                .whole
                .map(|distance| (distance, kept.powi(distance as i32)));
            let start = distances
                .prefix
                .filter(|_| open)
                .map(|distance| (distance, completed * kept.powi(distance as i32)));
            let best = match (whole, start) {
                (Some(whole), Some(start)) => Some((whole.0.min(start.0), whole.1.max(start.1))),
                (whole, start) => whole.or(start),
            };
            if let Some((distance, weight)) = best {
                found.push(Correction {
                    term,
                    distance,
                    weight,
                });
            }
        }
        let Some(nearest) = found.iter().map(|correction| correction.distance).min() else {
            return found;
        };
        found.retain(|correction| correction.distance == nearest);
        if found.len() > Self::MAX_CORRECTIONS {
            // Only the cap needs an order: strongest match, then the spelling
            // closest to what was typed, then the commonest term.
            let mut ranked: Vec<(f64, Correction)> = found
                .into_iter()
                .map(|correction| {
                    let term = &self.vocabulary.terms[correction.term];
                    (trigram_similarity_chars(word, term), correction)
                })
                .collect();
            ranked.sort_unstable_by(|(left_similarity, left), (right_similarity, right)| {
                right
                    .weight
                    .total_cmp(&left.weight)
                    .then(right_similarity.total_cmp(left_similarity))
                    .then(
                        self.postings
                            .of(right.term)
                            .len()
                            .cmp(&self.postings.of(left.term).len()),
                    )
                    .then(left.term.cmp(&right.term))
            });
            ranked.truncate(Self::MAX_CORRECTIONS);
            found = ranked
                .into_iter()
                .map(|(_, correction)| correction)
                .collect();
        }
        found
    }

    fn tally(&self, expansions: &[Expansion]) -> Tally {
        let count = self.ids.len();
        let mut tally = Tally {
            score: vec![0.0; count],
            covered: vec![0; count],
            exact: vec![0; count],
            fields: vec![0; count],
        };
        let mut best = vec![0.0f64; count];
        let mut touched = Vec::new();
        for (position, expansion) in expansions.iter().enumerate() {
            let bit = 1u32 << position;
            for found in &expansion.matches {
                let idf = self.idf(found.term);
                for posting in self.postings.of(found.term) {
                    let doc = posting.doc;
                    if tally.covered[doc] & bit == 0 {
                        tally.covered[doc] |= bit;
                        touched.push(doc);
                    }
                    if found.exact {
                        tally.exact[doc] |= bit;
                    }
                    tally.fields[doc] |= posting.fields;
                    best[doc] = f64::max(best[doc], found.weight * idf * posting.saturation);
                }
            }
            for doc in touched.drain(..) {
                tally.score[doc] += best[doc];
                best[doc] = 0.0;
            }
        }
        tally
    }

    fn idf(&self, term: usize) -> f64 {
        let documents = self.ids.len() as f64;
        let frequency = self.postings.of(term).len() as f64;
        (1.0 + (documents - frequency + 0.5) / (frequency + 0.5)).ln()
    }

    /// `1 + phrase_bonus · w_f / w_max` for the strongest field `f` where the
    /// query tokens appear together and in order, or 1 without such a field.
    fn phrase_factor(&self, doc: usize, expansions: &[Expansion]) -> f64 {
        self.spans_of(doc)
            .iter()
            .filter(|span| {
                self.sequence[span.start..span.end]
                    .windows(expansions.len())
                    .any(|window| {
                        window
                            .iter()
                            .zip(expansions)
                            .all(|(term, expansion)| expansion.contains(*term))
                    })
            })
            .map(|span| self.config.weight(span.field))
            .max_by(f64::total_cmp)
            .map_or(1.0, |weight| {
                1.0 + self.config.phrase_bonus * weight / self.config.strongest_weight()
            })
    }

    fn rank(
        &self,
        tally: &Tally,
        candidates: Vec<usize>,
        everything: u32,
        limit: usize,
    ) -> Vec<Hit> {
        let mut ranked: Vec<Ranked> = candidates
            .into_iter()
            .map(|doc| Ranked {
                matched_tokens: tally.covered[doc].count_ones(),
                score: tally.score[doc],
                doc,
            })
            .collect();
        let order = |left: &Ranked, right: &Ranked| {
            right
                .matched_tokens
                .cmp(&left.matched_tokens)
                .then(right.score.total_cmp(&left.score))
                .then(left.doc.cmp(&right.doc))
        };
        if ranked.len() > limit {
            ranked.select_nth_unstable_by(limit - 1, order);
            ranked.truncate(limit);
        }
        ranked.sort_unstable_by(order);
        ranked
            .into_iter()
            .map(|ranked| Hit {
                id: self.ids[ranked.doc].clone(),
                score: ranked.score,
                matched: Field::ALL
                    .into_iter()
                    .filter(|field| tally.fields[ranked.doc] & field.bit() != 0)
                    .collect(),
                exact: tally.exact[ranked.doc] == everything,
            })
            .collect()
    }
}

impl fmt::Debug for Index {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Index")
            .field("documents", &self.ids.len())
            .field("terms", &self.vocabulary.len())
            .field("postings", &self.postings.entries.len())
            .field("config", &self.config)
            .finish()
    }
}

/// Mean length of each field over the documents that have it; 0 for a field
/// no document has.
fn average_lengths(lengths: &[[usize; FIELD_COUNT]]) -> [f64; FIELD_COUNT] {
    let mut average = [0.0; FIELD_COUNT];
    for (index, average) in average.iter_mut().enumerate() {
        let present = lengths
            .iter()
            .map(|length| length[index])
            .filter(|length| *length > 0);
        let (total, count) = present.fold((0usize, 0usize), |(total, count), length| {
            (total + length, count + 1)
        });
        if count > 0 {
            *average = total as f64 / count as f64;
        }
    }
    average
}

impl Vocabulary {
    /// Sorts the distinct `texts` (indexed by provisional term id) and
    /// renumbers `sequence` from provisional to sorted ids.
    fn sorted(texts: &[String], sequence: &mut [usize]) -> Self {
        let mut order: Vec<usize> = (0..texts.len()).collect();
        order.sort_unstable_by(|left, right| texts[*left].cmp(&texts[*right]));
        let mut sorted_id = vec![0; texts.len()];
        for (sorted, provisional) in order.iter().enumerate() {
            sorted_id[*provisional] = sorted;
        }
        for term in sequence {
            *term = sorted_id[*term];
        }
        let terms: Vec<Box<[char]>> = order
            .iter()
            .map(|provisional| texts[*provisional].chars().collect())
            .collect();
        let masks = terms.iter().map(|term| char_mask(term)).collect();
        Self { terms, masks }
    }

    fn len(&self) -> usize {
        self.terms.len()
    }

    fn find(&self, word: &[char]) -> Option<usize> {
        self.terms
            .binary_search_by(|term| term.as_ref().cmp(word))
            .ok()
    }

    /// Terms longer than `word` that start with it.
    fn completions(&self, word: &[char]) -> Range<usize> {
        let start = self.terms.partition_point(|term| term.as_ref() <= word);
        let end = start + self.terms[start..].partition_point(|term| term.starts_with(word));
        start..end
    }
}

impl Postings {
    /// From `(term, posting)` pairs sorted by term.
    fn new(terms: usize, sorted: Vec<(usize, Posting)>) -> Self {
        let mut starts = vec![0; terms + 1];
        for (term, _) in &sorted {
            starts[term + 1] += 1;
        }
        let mut total = 0;
        for start in &mut starts {
            total += *start;
            *start = total;
        }
        Self {
            starts,
            entries: sorted.into_iter().map(|(_, posting)| posting).collect(),
        }
    }

    fn of(&self, term: usize) -> &[Posting] {
        &self.entries[self.starts[term]..self.starts[term + 1]]
    }
}

impl Expansion {
    fn contains(&self, term: usize) -> bool {
        self.matches
            .binary_search_by_key(&term, |found| found.term)
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, title: &str, artist: &str, album: &str) -> Document {
        Document {
            id: id.into(),
            fields: vec![
                (Field::Title, title.into()),
                (Field::Artist, artist.into()),
                (Field::Album, album.into()),
            ],
        }
    }

    fn library() -> Index {
        Index::build(
            &[
                track("hero", "Hero", "Enrique Iglesias", "Escape"),
                track("bailando", "Bailando", "Enrique Iglesias", "Sex and Love"),
                track("paradisio", "Bailando", "Paradisio", "Paradisio"),
                track("cancion", "Canción del Mariachi", "Los Lobos", "Desperado"),
                track("bamba", "La Bamba", "Ritchie Valens", "Ritchie Valens"),
                track("badly", "Badly", "Some Band", "Demos"),
            ],
            Bm25Config::default(),
        )
    }

    fn ids(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|hit| hit.id.as_str()).collect()
    }

    fn hit<'a>(hits: &'a [Hit], id: &str) -> &'a Hit {
        hits.iter().find(|hit| hit.id == id).expect("hit present")
    }

    #[test]
    fn title_outranks_album_for_the_same_term() {
        let index = Index::build(
            &[
                track("album", "Otro", "Nadie", "Amor"),
                track("title", "Amor", "Nadie", "Otro"),
            ],
            Bm25Config::default(),
        );
        let hits = index.search("amor", 10);
        assert_eq!(ids(&hits), ["title", "album"]);
        assert_eq!(hit(&hits, "title").matched, [Field::Title]);
        assert_eq!(hit(&hits, "album").matched, [Field::Album]);
        assert!(hits.iter().all(|hit| hit.exact));
    }

    #[test]
    fn every_token_must_match_some_field() {
        let hits = library().search("enrique bailando", 10);
        assert_eq!(ids(&hits), ["bailando"]);
        assert_eq!(hits[0].matched, [Field::Title, Field::Artist]);
        assert!(hits[0].exact);
    }

    #[test]
    fn last_token_completes_as_you_type() {
        let index = library();
        let hits = index.search("bai", 10);
        assert_eq!(ids(&hits), ["bailando", "paradisio"]);
        assert!(hits.iter().all(|hit| !hit.exact));
        assert_eq!(
            ids(&index.search("enrique iglesias bail", 10)),
            ["bailando"]
        );
        // A trailing separator means the word is finished.
        assert!(index.search("bai ", 10).is_empty());
        // Only the last token completes: «bai» matches nothing, so the hits
        // are the partial matches of «enrique» alone, in input order.
        let hits = index.search("bai enrique", 10);
        assert_eq!(ids(&hits), ["hero", "bailando"]);
    }

    #[test]
    fn exact_terms_outrank_completions() {
        let index = Index::build(
            &[
                track("amores", "Amores", "Nadie", "Otro"),
                track("amor", "Amor", "Nadie", "Otro"),
            ],
            Bm25Config::default(),
        );
        let hits = index.search("amor", 10);
        assert_eq!(ids(&hits), ["amor", "amores"]);
        assert!(hits[0].exact && !hits[1].exact);
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn typos_are_found_below_exact_matches() {
        let index = library();
        let exact = index.search("bailando", 10);
        let typo = index.search("bailnado", 10);
        assert_eq!(ids(&typo), ids(&exact));
        for (typo, exact) in typo.iter().zip(&exact) {
            assert!(!typo.exact && exact.exact);
            assert!(typo.score < exact.score);
        }
        let hits = index.search("enrrique", 10);
        assert_eq!(ids(&hits), ["hero", "bailando"]);
        assert!(hits.iter().all(|hit| !hit.exact));
        assert_eq!(ids(&index.search("enrrique bailnado", 10)), ["bailando"]);
        // A word still being typed is corrected against term beginnings.
        assert_eq!(ids(&index.search("enrique bial", 10)), ["bailando"]);
    }

    #[test]
    fn short_words_and_numbers_are_never_corrected() {
        let index = Index::build(
            &[
                track("2023", "Summer 2023", "Nadie", "Otro"),
                track("sol", "Sol", "Nadie", "Otro"),
            ],
            Bm25Config::default(),
        );
        assert!(index.search("2024", 10).is_empty());
        assert!(index.search("sal ", 10).is_empty());
        assert_eq!(ids(&index.search("summer 2023", 10)), ["2023"]);
    }

    #[test]
    fn accents_and_case_do_not_matter() {
        let index = library();
        assert_eq!(ids(&index.search("cancion", 10)), ["cancion"]);
        assert_eq!(ids(&index.search("CANCIÓN mariachi", 10)), ["cancion"]);
        let hits = index.search("canción", 10);
        assert!(hits[0].exact);
    }

    #[test]
    fn contiguous_phrases_get_a_bonus() {
        let index = Index::build(
            &[
                track("scattered", "La gente baila bamba", "Nadie", "Otro"),
                track("phrase", "Me gusta la bamba", "Nadie", "Otro"),
            ],
            Bm25Config::default(),
        );
        let hits = index.search("la bamba", 10);
        assert_eq!(ids(&hits), ["phrase", "scattered"]);
        let bonus = 1.0 + Bm25Config::default().phrase_bonus;
        assert!((hits[0].score / hits[1].score - bonus).abs() < 1e-9);
        // Reversed order is no phrase: equal scores keep the input order.
        assert_eq!(ids(&index.search("bamba la", 10)), ["scattered", "phrase"]);
    }

    #[test]
    fn empty_queries_and_zero_limits_return_nothing() {
        let index = library();
        for query in ["", "   ", "¡¿!?", " - "] {
            assert!(index.search(query, 10).is_empty(), "{query:?}");
        }
        assert!(index.search("bailando", 0).is_empty());
        assert!(Index::build(&[], Bm25Config::default())
            .search("bailando", 10)
            .is_empty());
    }

    #[test]
    fn missing_tokens_fall_back_to_the_best_partial_matches() {
        let hits = library().search("enrique bailando remix", 10);
        assert_eq!(ids(&hits), ["bailando", "paradisio", "hero"]);
        assert!(hits.iter().all(|hit| !hit.exact));
        assert_eq!(
            ids(&library().search("enrique bailando remix", 1)),
            ["bailando"]
        );
    }

    #[test]
    fn ties_keep_input_order_and_duplicate_ids_index_once() {
        let index = Index::build(
            &[
                track("b", "Same Song", "Same Artist", "Same Album"),
                track("a", "Same Song", "Same Artist", "Same Album"),
                track("b", "Other", "Other", "Other"),
            ],
            Bm25Config::default(),
        );
        assert_eq!(ids(&index.search("same song", 10)), ["b", "a"]);
        assert!(index.search("other", 10).is_empty());
    }

    #[test]
    fn zero_weight_fields_and_out_of_range_config_are_handled() {
        let config = Bm25Config {
            album_weight: 0.0,
            k1: f64::NAN,
            b: 7.0,
            typo_penalty: 1.0,
            ..Bm25Config::default()
        };
        let index = Index::build(
            &[track("song", "Hero", "Enrique Iglesias", "Escape")],
            config,
        );
        assert!(index.search("escape", 10).is_empty());
        assert!(index.search("enrrique", 10).is_empty());
        let hits = index.search("hero", 10);
        assert_eq!(ids(&hits), ["song"]);
        assert!(hits[0].score.is_finite() && hits[0].score > 0.0);
    }

    #[test]
    fn repeated_query_tokens_and_long_queries_are_bounded() {
        let index = Index::build(
            &[
                track("land", "La La Land", "Nadie", "Otro"),
                track("la", "La Vida", "Nadie", "Otro"),
            ],
            Bm25Config::default(),
        );
        assert_eq!(ids(&index.search("la la land", 10)), ["land"]);
        let long = "la ".repeat(Index::MAX_QUERY_TOKENS * 2);
        assert_eq!(index.search(&long, 10).len(), 2);
    }

    #[test]
    fn corrections_per_token_are_capped() {
        const IDEOGRAPHS: u32 = 0x4e00;
        let docs: Vec<Document> = (0..200)
            .filter_map(|offset| char::from_u32(IDEOGRAPHS + offset))
            .map(|letter| track(&format!("abc{letter}"), &format!("abc{letter}"), "x", "y"))
            .collect();
        let index = Index::build(&docs, Bm25Config::default());
        // Every title is one substitution away from the query.
        let hits = index.search("abc0 ", usize::MAX);
        assert_eq!(hits.len(), Index::MAX_CORRECTIONS);
        assert!(hits.iter().all(|hit| !hit.exact));
    }

    #[test]
    fn an_index_can_be_shared_between_threads() {
        fn shareable<T: Send + Sync>() {}
        shareable::<Index>();
    }

    /// xorshift64*: a fixed generator keeps the benchmark corpus identical.
    struct Random(u64);

    impl Random {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next() % bound as u64) as usize
        }
    }

    /// Draws ranks with probability proportional to 1 / (rank + 1), the way
    /// words and artists are used.
    struct Zipf(Vec<f64>);

    impl Zipf {
        fn new(size: usize) -> Self {
            let mut total = 0.0;
            Self(
                (0..size)
                    .map(|rank| {
                        total += 1.0 / (rank as f64 + 1.0);
                        total
                    })
                    .collect(),
            )
        }

        fn draw(&self, random: &mut Random) -> usize {
            let total = self.0[self.0.len() - 1];
            let target = random.next() as f64 / u64::MAX as f64 * total;
            self.0
                .partition_point(|cumulative| *cumulative < target)
                .min(self.0.len() - 1)
        }
    }

    const SYLLABLES: [&str; 40] = [
        "ba", "la", "do", "mi", "ra", "sol", "en", "ri", "que", "pe", "so", "plu", "ma", "can",
        "cion", "mor", "te", "quie", "ro", "ver", "dad", "no", "che", "luz", "co", "zon", "ti",
        "em", "po", "vi", "da", "fue", "go", "ca", "lle", "lu", "na", "sa", "bo", "gen",
    ];

    fn lexicon(random: &mut Random, size: usize) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut words = Vec::with_capacity(size);
        while words.len() < size {
            let syllables = 2 + random.below(3);
            let word: String = (0..syllables)
                .map(|_| SYLLABLES[random.below(SYLLABLES.len())])
                .collect();
            if seen.insert(word.clone()) {
                words.push(word);
            }
        }
        words
    }

    fn phrase(
        random: &mut Random,
        words: &[String],
        zipf: &Zipf,
        shortest: usize,
        longest: usize,
    ) -> String {
        let count = shortest + random.below(longest - shortest + 1);
        (0..count)
            .map(|_| words[zipf.draw(random)].as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn transpose(word: &str) -> String {
        let mut characters: Vec<char> = word.chars().collect();
        characters.swap(1, 2);
        characters.into_iter().collect()
    }

    fn drop_middle(word: &str) -> String {
        let mut characters: Vec<char> = word.chars().collect();
        characters.remove(characters.len() / 2);
        characters.into_iter().collect()
    }

    fn report(label: &str, mut timings: Vec<std::time::Duration>) -> std::time::Duration {
        timings.sort_unstable();
        let mean = timings.iter().sum::<std::time::Duration>() / timings.len() as u32;
        let at = |share: f64| timings[((timings.len() - 1) as f64 * share) as usize];
        println!(
            "{label}: mean {:.0} µs, p50 {:.0} µs, p95 {:.0} µs, max {:.0} µs",
            mean.as_secs_f64() * 1e6,
            at(0.5).as_secs_f64() * 1e6,
            at(0.95).as_secs_f64() * 1e6,
            at(1.0).as_secs_f64() * 1e6,
        );
        mean
    }

    #[test]
    #[ignore = "timing: cargo test -p yt-search --release -- --ignored --nocapture"]
    fn ten_thousand_documents_answer_typo_queries_in_well_under_five_milliseconds() {
        use std::time::{Duration, Instant};
        const DOCUMENTS: usize = 10_000;
        const LEXICON: usize = 100_000;
        const ARTISTS: usize = 1_500;
        const ALBUMS: usize = 3_000;
        const QUERIES: usize = 500;
        const LIMIT: usize = 25;
        const BUDGET: Duration = Duration::from_millis(5);

        let mut random = Random(0x9e37_79b9_7f4a_7c15);
        let words = lexicon(&mut random, LEXICON);
        let word_rank = Zipf::new(LEXICON);
        let artists: Vec<String> = (0..ARTISTS)
            .map(|_| phrase(&mut random, &words, &word_rank, 1, 3))
            .collect();
        let albums: Vec<String> = (0..ALBUMS)
            .map(|_| phrase(&mut random, &words, &word_rank, 1, 4))
            .collect();
        let (artist_rank, album_rank) = (Zipf::new(ARTISTS), Zipf::new(ALBUMS));
        let docs: Vec<Document> = (0..DOCUMENTS)
            .map(|index| {
                let title = phrase(&mut random, &words, &word_rank, 2, 5);
                let artist = artists[artist_rank.draw(&mut random)].clone();
                let album = albums[album_rank.draw(&mut random)].clone();
                track(&format!("track-{index}"), &title, &artist, &album)
            })
            .collect();

        let started = Instant::now();
        let index = Index::build(&docs, Bm25Config::default());
        println!(
            "build: {:.1} ms, {index:?}",
            started.elapsed().as_secs_f64() * 1e3
        );

        let mut typo_timings = Vec::with_capacity(QUERIES);
        let mut exact_timings = Vec::with_capacity(QUERIES);
        let mut prefix_timings = Vec::with_capacity(QUERIES);
        let mut found = 0;
        for _ in 0..QUERIES {
            let target = &docs[random.below(DOCUMENTS)];
            let title = tokens(&target.fields[0].1);
            let artist = tokens(&target.fields[1].1);
            let typo_query = format!(
                "{} {} {}",
                transpose(&title[0]),
                drop_middle(&title[1]),
                artist[0]
            );
            let started = Instant::now();
            let hits = index.search(&typo_query, LIMIT);
            typo_timings.push(started.elapsed());
            found += usize::from(hits.iter().any(|hit| hit.id == target.id));

            let exact_query = format!("{} {} {}", title[0], title[1], artist[0]);
            let started = Instant::now();
            std::hint::black_box(index.search(&exact_query, LIMIT));
            exact_timings.push(started.elapsed());

            let prefix_query: String = title[0].chars().take(2).collect();
            let started = Instant::now();
            std::hint::black_box(index.search(&prefix_query, LIMIT));
            prefix_timings.push(started.elapsed());
        }
        println!("typo queries find their target in the top {LIMIT}: {found}/{QUERIES}");
        let typo_mean = report("3 tokens, 2 with typos", typo_timings);
        report("3 exact tokens", exact_timings);
        report("2-letter prefix", prefix_timings);
        assert!(typo_mean < BUDGET, "{typo_mean:?}");
    }
}
