# yt-search

Pure search for YT Ultimate: text folding, BM25F relevance with as-you-type completion and typo tolerance, query
auto-completion, rank fusion, personal re-ranking and the choice of the top result. The application passes typed values
it already validated (library tracks, online results it fetched, the user's search history and affinities) and renders
the orderings this crate returns; the interface decides nothing about relevance. The crate performs no file, network or
database access, never reads the clock (time is a parameter), uses no randomness, and returns equal output for equal
input. It depends on `yt-core`, whose `fold` it re-exports so every crate compares text the same way, and on
`unicode-normalization` for word boundaries.

## API

| Item | Contract |
|---|---|
| `fold(text) -> String` | NFKD, combining marks removed, lowercase, apostrophe variants (’, ‘, ʼ, ´ and the grave accent) written `'`, whitespace runs collapsed to one space and trimmed. Latin letters without a decomposition are spelled out (ß → ss, æ → ae, œ → oe, ø → o, đ/ð → d, ł → l, ı → i, þ → th). «Canción» (composed or decomposed) and «CANCION» → `cancion`; «Ñandú» → `nandu`; «ﬁ» → `fi`; full-width → ASCII. Total, O(len). |
| `tokens(text) -> Vec<String>` | `fold`, split at every character that is not a letter or digit, no empty tokens. An apostrophe between letters or digits joins them («Don't» → `dont`). |
| `typo_budget(token_len) -> usize` | Typos tolerated by a token of `token_len` characters: 0 below `ONE_TYPO_MIN_CHARS` (4), 1 below `TWO_TYPOS_MIN_CHARS` (8), 2 from there on. Searches give purely numeric tokens 0. |
| `edit_distance_within(a, b, max) -> Option<usize>` | Optimal string alignment distance (insert, delete, substitute, transpose adjacent; no substring edited twice) in Unicode scalar values, `Some` only when ≤ `max`. Rejects length differences > `max` without work, fills only the diagonal band of width 2·max + 1 and stops at the first row beyond the bound. O(len · (2·max + 1)) time, O(len) memory. Compares characters as given. |
| `trigram_similarity(a, b) -> f64` | Jaccard index of padded character trigram sets, in [0, 1]; 1 for equal strings, symmetric. Orders typo corrections of equal distance. |
| `Field::{Title, Artist, Album, Other}`, `Field::ALL` | Where a text belongs; fields differ only by weight. |
| `Document { id, fields: Vec<(Field, String)> }` | One searchable item. A field may repeat (values count together for length, separately for phrases). |
| `Bm25Config` (+ `Default`, `weight(field)`) | `k1`, `b`, four field weights, `prefix_penalty`, `typo_penalty`, `phrase_bonus` (see Parameters). Out-of-range values are clamped and non-finite ones replaced by the default when the index is built. |
| `Index::build(&[Document], Bm25Config) -> Index` | Immutable inverted index. The first document with an id wins; a field of weight 0 is not indexed. `Index` is `Send + Sync`. |
| `Index::search(&self, query, limit) -> Vec<Hit>` | At most `limit` hits, best first (rules below). Empty query (no tokens) or `limit` 0 → empty. |
| `Hit { id, score, matched: Vec<Field>, exact }` | `score` is positive and comparable only within one search; `matched` lists fields in `Field::ALL` order; `exact` means every query token matched without completion or correction. |
| `Index::MAX_QUERY_TOKENS` (16), `Index::MAX_CORRECTIONS` (128) | Query tokens considered; typo corrections kept per token. |
| `complete(prefix, &[HistoryQuery], entities, provider, now, &CompletionConfig, limit) -> Vec<Suggestion>` | Completions in display order: history, then library entities, then provider (rules below). Empty prefix → recent history. Each folded text once. |
| `HistoryQuery { text, uses, last_at }` | A past search as `yt-core` stores it; `last_at` in Unix seconds. |
| `Suggestion { text, source, score }`, `SuggestionSource::{History, Library, Provider}` | `text` trimmed; `score` non-negative, comparable only within one source. |
| `CompletionConfig { history_half_life_days, inner_word_weight }` (+ `Default`) | Sanitized like `Bm25Config`. |
| `reciprocal_rank_fusion(&[Vec<String>], k) -> Vec<(String, f64)>` | Σ 1 / (k + rank) per id, ranks from 1 among a list's distinct ids. Ties: best rank reached, then earlier list (`[a, b]` + `[x, y]` → `a, x, b, y`). Negative or non-finite `k` → `DEFAULT_RRF_K` (60). |
| `personalize(scored, affinity, weight) -> Vec<(String, f64)>` | Adds `weight · tanh(affinity(id))`, a boost bounded by ±`weight` (in score units), re-sorts best first, ties keep input order. An id never passes one that scored `2 · weight` more (`weight` more when that one is neutral). Negative or non-finite `weight`, or a NaN affinity, means no boost. |
| `top_result(query, artists, songs, albums, library_best) -> Option<(TopKind, usize)>` | `TopKind::{Artist, Song, Album, Library}` and the index into the matching slice (0 for `Library`, the best library hit). Rules below. |

Typical use by the application, as a sketch in which every name not exported by this crate is the caller's
(`search` and `top_result` take well under a millisecond; `complete` is linear in the names passed, see Performance):

```rust
let index = Index::build(&library_documents, Bm25Config::default()); // on library change
let hits = index.search(&query, LIBRARY_LIMIT);
let fused = reciprocal_rank_fusion(&[hit_ids, provider_ids], DEFAULT_RRF_K);
let ranked = personalize(fused, |id| profile_score(id), PERSONAL_WEIGHT);
let top = top_result(&query, &artists, &songs, &albums, hits.first().map(|hit| hit.score));
let suggestions = complete(&typed, &history, &names, &provider_suggestions, now, &CompletionConfig::default(), 8);
```

## Algorithms

**Folding.** Every comparison uses `fold`, implemented once in `yt-core` (`normalize.rs`, where its tests live) and
re-exported here. Characters decompose one at a time with NFKD (UAX #15); because NFKD only
reorders combining marks and all of them are dropped, this equals decomposing the whole text (a test checks the
equivalence and that every character with a non-zero combining class is a mark). Plain ASCII without whitespace runs
is copied and lowercased in bulk. Apostrophes join words as in UAX #29 word boundaries (a `MidLetter` between letters).

**Relevance (BM25F).** The simple BM25F of Robertson, Zaragoza & Taylor (2004) as presented by Robertson & Zaragoza
(2009): each field's term frequency is normalized by the field's length, weighted and summed into one pseudo-frequency
that saturates once, so repeating a word across fields never counts linearly.

```text
tf'(t, d)   = Σ_f  w_f · tf(t, d, f) / ((1 − b) + b · len(d, f) / avglen(f))
score(q, d) = Σ_q  max over the terms t that q matches of  penalty(q, t) · idf(t) · tf'(t, d) / (k1 + tf'(t, d))
idf(t)      = ln(1 + (N − df(t) + 0.5) / (df(t) + 0.5))
```

`avglen(f)` is averaged over the documents that have field `f`; the `+ 1` inside the logarithm keeps idf positive for
terms present in more than half of the documents. Saturations are precomputed per posting at build time, so a search
only multiplies `penalty · idf · saturation`.

**As-you-type and typos.** Each query token is expanded into vocabulary terms before scoring:

1. The exact term (penalty 1).
2. The last token, only when nothing follows it in the query (`"bad "` is a finished word, `"bad"` is not), also
   matches every longer term it starts, a binary-searched run of the sorted vocabulary (penalty `1 − prefix_penalty`).
3. A token that neither equals nor, as the last token, starts any term is corrected: it matches the terms at the
   smallest optimal string alignment distance within its typo budget (Damerau 1964; Levenshtein 1966), and the last
   token also matches terms whose beginning is that close (prefix distance, the minimum of the last row of the same
   dynamic program). Penalty `(1 − typo_penalty)^edits`, times `1 − prefix_penalty` for a beginning. Numbers are
   never corrected (a near year is a different year), nor tokens under four characters. The vocabulary scan rejects
   most terms by length and by a 64-bit character-class signature before the banded dynamic program runs (Ukkonen
   1985; signature filtering as surveyed by Boytsov 2011). Only the nearest distance is kept, and when more than
   `MAX_CORRECTIONS` terms remain, the strongest matches win, then the highest trigram similarity to what was typed
   (Angell, Freund & Willett 1983), then the commonest term.

A token contributes its best expansion per document, so a document holding several variants is not rewarded twice.

**AND, fallback, phrases, ties.** A document must match every token. When no document does, the search falls back to
the documents matching the most tokens, ordered by that count, then by score (`exact` is then false). With full
matches and two or more tokens, a document whose tokens appear together and in order in one field value is multiplied
by `1 + phrase_bonus · w_f / w_max`, `f` being the strongest such field. Equal keys keep the order documents were given
to `Index::build`. Top-k uses selection (`select_nth_unstable`), not a full sort.

**Query auto-completion.** Past searches rank by Most Popular Completion (Bar-Yossef & Kraus 2011) over the user's own
log, with popularity decayed by age as in time-sensitive completion (Shokouhi & Radinsky 2012): searches whose folds
are equal merge, and each scores `match weight × Σ uses · ½^(age / half-life)`. A text matches when its fold starts
with the folded prefix (weight 1) or a later word does (`inner_word_weight`). An empty prefix lists the most recent
searches, scored `½^(age / half-life)`. Library entities that match follow, scored `match weight × occurrences` in the
list given (an artist with many tracks first), then shorter, then by fold. Provider suggestions come last in their
order (`1 / (1 + position)`); they are not matched against the prefix because the provider may correct its spelling.
A future `last_at` counts as now; a NaN one weighs nothing.

**Fusion and personalization.** Reciprocal rank fusion (Cormack, Clarke & Büttcher 2009) merges lists from
incomparable scorers (library BM25F, provider order) by rank alone. Personal re-ranking follows Teevan, Dumais &
Horvitz (2005): the user's affinity (for example from `yt-discovery`) adds a bounded `weight · tanh(affinity)`, so
liked items pass close competitors and disliked ones sink, while gaps of `2 · weight` or more survive. With RRF
scores, choose `weight` as a fraction of `1 / (k + 1)` (one rank step near the top is about `1 / (k + 1)²`).

**Top result.** The first rule that holds wins:

1. Artist: the artist's name folded, without spaces or punctuation («Peso Pluma» → `pesopluma`), within
   `typo_budget` of the query taken the same way (0 for a number). Fewer edits, then the earlier artist.
2. Song: every query word matches a different title word, exactly, within that word's typo budget, or (last word
   still being typed) as the start of a title word. Fewer typos, then words together and in order, then the earlier
   title.
3. Album: the same rule over album titles.
4. Library: `library_best` (the best `Hit::score`) when positive and finite.

## Parameters

| Parameter | Default | Range | Meaning |
|---|---|---|---|
| `Bm25Config::k1` | 1.2 | ≥ 0 | Term-frequency saturation; 0 counts presence only. |
| `Bm25Config::b` | 0.75 | [0, 1] | Field-length normalization. |
| `title_weight`, `artist_weight`, `album_weight`, `other_weight` | 3.0, 2.0, 1.2, 0.6 | ≥ 0 | BM25F field weights; 0 leaves the field unindexed. |
| `prefix_penalty` | 0.25 | [0, 1] | A completion keeps 75 % of a match; 1 disables completions. |
| `typo_penalty` | 0.4 | [0, 1] per edit | One edit keeps 60 %, two keep 36 %; 1 disables corrections. |
| `phrase_bonus` | 0.5 | ≥ 0 | Up to +50 % for a contiguous, ordered phrase in the title. |
| `CompletionConfig::history_half_life_days` | 14 days | > 0 | Age at which a past search counts half; search interests are short-lived. |
| `CompletionConfig::inner_word_weight` | 0.5 | [0, 1] | Weight of a later-word match; 0 disables them. |
| `DEFAULT_RRF_K` | 60 | ≥ 0 | RRF rank constant (Cormack et al. 2009). |
| `ONE_TYPO_MIN_CHARS`, `TWO_TYPOS_MIN_CHARS` | 4, 8 characters | | Typo budget thresholds. |
| `Index::MAX_QUERY_TOKENS` | 16 tokens | | Later tokens are ignored. |
| `Index::MAX_CORRECTIONS` | 128 terms | | Typo corrections kept per token. |

## Trust boundary

Inputs are typed values the application already validated: library rows from `yt-core`, provider results parsed by
`yt-media`, search history from `yt-core` (at most 300 entries of at most 200 characters), affinities from
`yt-discovery`. This crate still accepts any string and any number without panicking: text is only compared, never
interpreted; non-finite or out-of-range configuration, `k`, weights, affinities and timestamps are sanitized as
documented per item. Outputs contain only ids and texts that were passed in; ranking never creates identities. Nothing
is logged, persisted or sent. Ids are opaque strings; a repeated id is indexed once and fused once per list.

## Limits

- Tokens are runs of letters and digits. Scripts written without spaces (Chinese, Japanese, Thai) are not segmented:
  a run is one token, found whole or by its start while typing.
- Dropping combining marks also drops Indic vowel signs and kana voicing marks (が folds to か), which widens matches in
  those scripts. Hangul decomposes to jamo, consistently on both sides.
- Typo tolerance works per token: joined or split words («acdc» for «AC/DC») are not corrected by `Index::search`
  (the top-result artist rule compares compact names, so «badbunny» still finds the artist «Bad Bunny»).
- A token equal to some vocabulary term is never corrected, so a correctly spelled query does not reach a misspelled
  title; the partial fallback covers queries whose AND fails.
- Corrections scan the vocabulary linearly behind cheap filters: right for a personal library (tens of thousands of
  terms). A corpus with millions of terms would need an automaton or a deletion index instead.
- The index is immutable; rebuild it (off the UI thread) when the library changes.
- `complete` folds every entity on each call, O(entities); pass distinct names.

## Performance

Measured on the development Mac (Apple Silicon, release build) with
`cargo test -p yt-search --release -- --ignored --nocapture`, over a synthetic Zipf-distributed corpus of 10 000
tracks (13 104 distinct terms, 78 517 postings):

| Operation | Time |
|---|---|
| `Index::build` | 14–20 ms |
| 3-token query, 2 tokens with typos (500 queries) | mean ≈ 0.10 ms, p95 ≈ 0.16 ms, max ≈ 0.21 ms; the target track is in the top 25 for 496/500 |
| 3 exact tokens | mean ≈ 19 µs |
| 2-letter prefix | mean ≈ 48 µs |
| `complete` over 20 000 names and 300 searches, a third of the names matching | ≈ 1.7 ms |

A search allocates five per-document arrays (25 bytes per document, about 250 KB for 10 000 tracks) and one row
buffer for the edit-distance program. The index holds each term once as characters plus a 64-bit signature, one
24-byte posting per (term, document) and one term id per token, a few megabytes for 10 000 tracks.

## Verification

`cargo test -p yt-search` covers: tokenization and apostrophes (folding itself is tested in `yt-core`); distances against an unbounded
reference (whole and prefix) for every bound, the early exit and the character-class lower bound; typo budgets and
trigram similarity; BM25F title over album, multi-field AND, prefix completion («bai» → «Bailando», not after a
space), typos («bailnado», «enrrique», «bial») found below exact, completions below exact, numbers and short words
never corrected, accent-insensitive search («cancion» → «Canción»), phrase bonus and its order, empty queries, partial
fallback, tie order, duplicate ids, sanitized configuration, repeated and long queries, the correction cap and
thread sharing; completion order (frequent and recent first, decay over time, library fill, provider last, dedupe,
limit, inner words, sanitized configuration); RRF on known lists, ties, repeats and `k`; personalization that lifts
liked items without overturning large gaps; and every top-result rule («peso pluma» picks the artist «Peso Pluma»
when it exists and a song otherwise). Timings run with
`cargo test -p yt-search --release -- --ignored --nocapture`.

## References

- S. Robertson and H. Zaragoza. The Probabilistic Relevance Framework: BM25 and Beyond. *Foundations and Trends in
  Information Retrieval* 3(4):333–389, 2009.
- S. Robertson, H. Zaragoza and M. Taylor. Simple BM25 Extension to Multiple Weighted Fields. *Proceedings of the
  13th ACM International Conference on Information and Knowledge Management (CIKM)*, 42–49, 2004.
- F. J. Damerau. A Technique for Computer Detection and Correction of Spelling Errors. *Communications of the ACM*
  7(3):171–176, 1964.
- V. I. Levenshtein. Binary Codes Capable of Correcting Deletions, Insertions, and Reversals. *Soviet Physics
  Doklady* 10(8):707–710, 1966.
- E. Ukkonen. Algorithms for Approximate String Matching. *Information and Control* 64(1–3):100–118, 1985.
- L. Boytsov. Indexing Methods for Approximate Dictionary Searching: Comparative Analysis. *ACM Journal of
  Experimental Algorithmics* 16, article 1.1, 2011.
- R. C. Angell, G. E. Freund and P. Willett. Automatic Spelling Correction Using a Trigram Similarity Measure.
  *Information Processing & Management* 19(4):255–261, 1983.
- Z. Bar-Yossef and N. Kraus. Context-Sensitive Query Auto-Completion. *Proceedings of the 20th International
  Conference on World Wide Web (WWW)*, 107–116, 2011.
- M. Shokouhi and K. Radinsky. Time-Sensitive Query Auto-Completion. *Proceedings of the 35th International ACM SIGIR
  Conference on Research and Development in Information Retrieval*, 601–610, 2012.
- G. V. Cormack, C. L. A. Clarke and S. Büttcher. Reciprocal Rank Fusion Outperforms Condorcet and Individual Rank
  Learning Methods. *Proceedings of the 32nd International ACM SIGIR Conference on Research and Development in
  Information Retrieval*, 758–759, 2009.
- J. Teevan, S. T. Dumais and E. Horvitz. Personalizing Search via Automated Analysis of Interests and Activities.
  *Proceedings of the 28th International ACM SIGIR Conference on Research and Development in Information Retrieval*,
  449–456, 2005.
- Unicode Standard Annex #15, Unicode Normalization Forms; Unicode Standard Annex #29, Unicode Text Segmentation.
