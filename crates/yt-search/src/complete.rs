//! Query auto-completion: the user's own past searches first, then names
//! from the library, then the online provider's suggestions.
//!
//! Past searches rank by Most Popular Completion (Bar-Yossef & Kraus 2011)
//! over the user's own log, with popularity decayed by age as in
//! time-sensitive completion (Shokouhi & Radinsky 2012).

use crate::normalize::{fold, fold_into};
use std::collections::{HashMap, HashSet};

const SECONDS_PER_DAY: f64 = 86_400.0;

/// A search the user ran before, as `yt-core` remembers it.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryQuery {
    /// The search as the user last typed it.
    pub text: String,
    /// Times the user ran it.
    pub uses: u32,
    /// When the user last ran it, in Unix seconds.
    pub last_at: f64,
}

/// Where a suggestion comes from. Suggestions are listed in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SuggestionSource {
    /// A search the user ran before.
    History,
    /// An artist or title in the user's library.
    Library,
    /// A suggestion of the online provider.
    Provider,
}

/// One completion offered under the search box.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// Text to show and to search when chosen, trimmed.
    pub text: String,
    /// Where it comes from.
    pub source: SuggestionSource,
    /// Rank within its source, non-negative and higher first; scores of
    /// different sources are not comparable.
    pub score: f64,
}

/// Parameters of [`complete`].
///
/// [`complete`] clamps every value into its documented range and replaces a
/// non-finite value with its default.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompletionConfig {
    /// Days after which a past search counts half as much; positive.
    pub history_half_life_days: f64,
    /// Weight, in [0, 1], of a match that starts at a later word («pluma»
    /// for «Peso Pluma») relative to a match at the start; 0 turns such
    /// matches off.
    pub inner_word_weight: f64,
}

impl Default for CompletionConfig {
    /// A 14-day half-life, because search interests are short-lived, and
    /// inner-word matches at half weight.
    fn default() -> Self {
        Self {
            history_half_life_days: 14.0,
            inner_word_weight: 0.5,
        }
    }
}

impl CompletionConfig {
    fn sanitized(self) -> Self {
        let fallback = Self::default();
        Self {
            history_half_life_days: if self.history_half_life_days.is_finite()
                && self.history_half_life_days > 0.0
            {
                self.history_half_life_days
            } else {
                fallback.history_half_life_days
            },
            inner_word_weight: if self.inner_word_weight.is_finite() {
                self.inner_word_weight.clamp(0.0, 1.0)
            } else {
                fallback.inner_word_weight
            },
        }
    }
}

/// Completions for `prefix`, at most `limit`, in display order.
///
/// Texts compare by their [`fold`], so case, accents and spacing never
/// matter, and each folded text appears once, from the first source that
/// offers it. A text matches when it starts with the folded prefix, or at
/// `inner_word_weight` when one of its later words does.
///
/// 1. History: past searches (merged when their folds are equal) that
///    match, scored by match weight × Σ `uses` · ½^(age / half-life), so
///    frequent and recent searches come first. An empty prefix lists the
///    most recent searches instead, scored by ½^(age / half-life).
/// 2. Library: the matching `entities` (artist names and titles), scored by
///    match weight × how many times the name appears in `entities`, then
///    shorter first.
/// 3. Provider: the `provider` suggestions in their order, scored
///    1 / (1 + position). They are trusted to belong to this prefix, so
///    they are not matched against it (they may correct its spelling).
///
/// `now` and `last_at` are Unix seconds; a search from the future counts as
/// now and one with an unknown time weighs nothing. Ties break by fold. Time
/// O((history + entities) · text length).
pub fn complete(
    prefix: &str,
    history: &[HistoryQuery],
    entities: &[&str],
    provider: &[String],
    now: f64,
    config: &CompletionConfig,
    limit: usize,
) -> Vec<Suggestion> {
    let config = config.sanitized();
    let prefix = fold(prefix);
    let past = past_searches(
        history,
        now,
        config.history_half_life_days * SECONDS_PER_DAY,
    );
    let mut list = SuggestionList::new(limit);
    if prefix.is_empty() {
        let mut recent: Vec<&PastSearch> = past.iter().collect();
        recent.sort_by(|left, right| {
            right
                .recency
                .total_cmp(&left.recency)
                .then(right.popularity.total_cmp(&left.popularity))
                .then(left.key.cmp(&right.key))
        });
        for search in recent {
            list.push(
                &search.key,
                &search.text,
                SuggestionSource::History,
                search.recency,
            );
        }
        return list.items;
    }

    let mut searches: Vec<(f64, &PastSearch)> = past
        .iter()
        .filter_map(|search| {
            match_weight(&search.key, &prefix, config.inner_word_weight)
                .map(|weight| (weight * search.popularity, search))
        })
        .collect();
    searches.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .total_cmp(left_score)
            .then(right.recency.total_cmp(&left.recency))
            .then(left.key.cmp(&right.key))
    });
    for (score, search) in searches {
        list.push(&search.key, &search.text, SuggestionSource::History, score);
    }
    if list.is_full() {
        return list.items;
    }

    let names = library_names(entities, &prefix, config.inner_word_weight);
    let mut names: Vec<(f64, &LibraryName)> = names
        .iter()
        .map(|name| (name.weight * name.count as f64, name))
        .collect();
    let order = |(left_score, left): &(f64, &LibraryName),
                 (right_score, right): &(f64, &LibraryName)| {
        right_score
            .total_cmp(left_score)
            .then(left.length.cmp(&right.length))
            .then(left.key.cmp(&right.key))
    };
    // Each name repeats at most one listed search, so `limit` names suffice.
    if names.len() > limit {
        names.select_nth_unstable_by(limit - 1, order);
        names.truncate(limit);
    }
    names.sort_unstable_by(order);
    for (score, name) in names {
        list.push(&name.key, name.text, SuggestionSource::Library, score);
    }

    for (position, text) in provider.iter().enumerate() {
        let score = 1.0 / (1.0 + position as f64);
        list.push(&fold(text), text, SuggestionSource::Provider, score);
    }
    list.items
}

/// Past searches merged by fold.
struct PastSearch {
    key: String,
    /// Text of the most recent variant.
    text: String,
    /// Σ uses · decay over the merged variants.
    popularity: f64,
    /// Decay of the most recent use.
    recency: f64,
}

struct LibraryName<'a> {
    key: String,
    /// Characters in `key`; shorter names rank first among equals.
    length: usize,
    /// Text of the first occurrence.
    text: &'a str,
    count: usize,
    /// [`match_weight`] of the key.
    weight: f64,
}

/// Suggestions in display order, without repeated folds, up to a limit.
struct SuggestionList {
    items: Vec<Suggestion>,
    keys: HashSet<String>,
    limit: usize,
}

impl SuggestionList {
    fn new(limit: usize) -> Self {
        Self {
            items: Vec::new(),
            keys: HashSet::new(),
            limit,
        }
    }

    fn is_full(&self) -> bool {
        self.items.len() >= self.limit
    }

    fn push(&mut self, key: &str, text: &str, source: SuggestionSource, score: f64) {
        if self.is_full() || key.is_empty() || !self.keys.insert(key.to_owned()) {
            return;
        }
        self.items.push(Suggestion {
            text: text.trim().to_owned(),
            source,
            score,
        });
    }
}

fn past_searches(history: &[HistoryQuery], now: f64, half_life_seconds: f64) -> Vec<PastSearch> {
    let mut merged: Vec<PastSearch> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();
    for query in history {
        let key = fold(&query.text);
        if key.is_empty() {
            continue;
        }
        let recency = decay(now - query.last_at, half_life_seconds);
        let popularity = f64::from(query.uses) * recency;
        if let Some(position) = positions.get(&key) {
            let search = &mut merged[*position];
            search.popularity += popularity;
            if recency > search.recency {
                search.recency = recency;
                search.text.clone_from(&query.text);
            }
            continue;
        }
        positions.insert(key.clone(), merged.len());
        merged.push(PastSearch {
            key,
            text: query.text.clone(),
            popularity,
            recency,
        });
    }
    merged
}

/// Entities matching `prefix`, merged by fold and counted. Only matching
/// folds are kept, so a large library costs one fold per name.
fn library_names<'a>(
    entities: &[&'a str],
    prefix: &str,
    inner_word_weight: f64,
) -> Vec<LibraryName<'a>> {
    let mut names: Vec<LibraryName<'a>> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();
    let mut key = String::new();
    for entity in entities {
        fold_into(entity, &mut key);
        let Some(weight) = match_weight(&key, prefix, inner_word_weight) else {
            continue;
        };
        if let Some(position) = positions.get(key.as_str()) {
            names[*position].count += 1;
            continue;
        }
        positions.insert(key.clone(), names.len());
        names.push(LibraryName {
            key: key.clone(),
            length: key.chars().count(),
            text: entity,
            count: 1,
            weight,
        });
    }
    names
}

/// Weight of something last used `age_seconds` ago: 1 now, ½ after one
/// half-life. A future time counts as now; an unknown age weighs nothing.
fn decay(age_seconds: f64, half_life_seconds: f64) -> f64 {
    if age_seconds.is_nan() {
        return 0.0;
    }
    0.5_f64.powf(age_seconds.max(0.0) / half_life_seconds)
}

/// 1 when folded `text` starts with folded `prefix`, `inner_word_weight` when
/// one of its later words does, `None` otherwise or for a zero weight.
fn match_weight(text: &str, prefix: &str, inner_word_weight: f64) -> Option<f64> {
    if text.starts_with(prefix) {
        return Some(1.0);
    }
    if inner_word_weight <= 0.0 {
        return None;
    }
    let mut previous: Option<char> = None;
    for (index, character) in text.char_indices() {
        if previous.is_some_and(|before| !before.is_alphanumeric())
            && text[index..].starts_with(prefix)
        {
            return Some(inner_word_weight);
        }
        previous = Some(character);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1_800_000_000.0;

    fn past(text: &str, uses: u32, days_ago: f64) -> HistoryQuery {
        HistoryQuery {
            text: text.into(),
            uses,
            last_at: NOW - days_ago * SECONDS_PER_DAY,
        }
    }

    fn texts(suggestions: &[Suggestion]) -> Vec<&str> {
        suggestions
            .iter()
            .map(|suggestion| suggestion.text.as_str())
            .collect()
    }

    fn sources(suggestions: &[Suggestion]) -> Vec<SuggestionSource> {
        suggestions
            .iter()
            .map(|suggestion| suggestion.source)
            .collect()
    }

    fn history_only(prefix: &str, history: &[HistoryQuery], now: f64) -> Vec<Suggestion> {
        complete(
            prefix,
            history,
            &[],
            &[],
            now,
            &CompletionConfig::default(),
            10,
        )
    }

    #[test]
    fn empty_prefix_lists_recent_searches() {
        let history = [
            past("bad bunny", 40, 30.0),
            past("Peso Pluma", 1, 0.1),
            past("karol g", 3, 2.0),
        ];
        let suggestions = history_only("  ", &history, NOW);
        assert_eq!(texts(&suggestions), ["Peso Pluma", "karol g", "bad bunny"]);
        assert!(suggestions
            .windows(2)
            .all(|pair| pair[0].score > pair[1].score));
        assert!(suggestions
            .iter()
            .all(|suggestion| suggestion.source == SuggestionSource::History));
    }

    #[test]
    fn frequent_and_recent_searches_come_first() {
        let history = [
            past("bailando", 1, 2.0),
            past("bad bunny", 12, 1.0),
            past("banda ms", 3, 1.0),
            past("hero", 50, 0.0),
        ];
        let suggestions = history_only("ba", &history, NOW);
        assert_eq!(texts(&suggestions), ["bad bunny", "banda ms", "bailando"]);
        // Searches used often long ago yield to recent ones.
        let history = [past("bad bunny", 20, 90.0), past("bailando", 2, 1.0)];
        assert_eq!(
            texts(&history_only("ba", &history, NOW)),
            ["bailando", "bad bunny"]
        );
    }

    #[test]
    fn popularity_decays_with_time() {
        let history = [past("bad bunny", 4, 0.0)];
        let today = history_only("bad", &history, NOW)[0].score;
        let half_life_later =
            NOW + CompletionConfig::default().history_half_life_days * SECONDS_PER_DAY;
        let later = history_only("bad", &history, half_life_later)[0].score;
        assert!((today - 4.0).abs() < 1e-9);
        assert!((later - 2.0).abs() < 1e-9);
        // A clock behind the stored time counts the search as current.
        assert!((history_only("bad", &history, NOW - SECONDS_PER_DAY)[0].score - 4.0).abs() < 1e-9);
        let unknown = [HistoryQuery {
            last_at: f64::NAN,
            ..past("bad bunny", 4, 0.0)
        }];
        assert_eq!(history_only("bad", &unknown, NOW)[0].score, 0.0);
    }

    #[test]
    fn library_fills_after_history_and_provider_comes_last() {
        let history = [past("Peso Pluma", 2, 1.0)];
        let entities = [
            "Pedro Infante",
            "peso pluma",
            "Perfecta",
            "Pedro Infante",
            "Bailando",
        ];
        let provider = ["PESO PLUMA".to_owned(), "pepe aguilar".to_owned()];
        let suggestions = complete(
            "pe",
            &history,
            &entities,
            &provider,
            NOW,
            &CompletionConfig::default(),
            10,
        );
        assert_eq!(
            texts(&suggestions),
            ["Peso Pluma", "Pedro Infante", "Perfecta", "pepe aguilar"]
        );
        assert_eq!(
            sources(&suggestions),
            [
                SuggestionSource::History,
                SuggestionSource::Library,
                SuggestionSource::Library,
                SuggestionSource::Provider
            ]
        );
        let limited = complete(
            "pe",
            &history,
            &entities,
            &provider,
            NOW,
            &CompletionConfig::default(),
            2,
        );
        assert_eq!(texts(&limited), ["Peso Pluma", "Pedro Infante"]);
        assert!(complete(
            "pe",
            &history,
            &entities,
            &provider,
            NOW,
            &CompletionConfig::default(),
            0
        )
        .is_empty());
    }

    #[test]
    fn matches_ignore_accents_and_reach_later_words() {
        let entities = ["Canción del Mariachi", "Peso Pluma", "Plumas", "Lupita"];
        let suggestions = complete(
            "CANCIO",
            &[],
            &entities,
            &[],
            NOW,
            &CompletionConfig::default(),
            10,
        );
        assert_eq!(texts(&suggestions), ["Canción del Mariachi"]);
        let suggestions = complete(
            "pluma",
            &[],
            &entities,
            &[],
            NOW,
            &CompletionConfig::default(),
            10,
        );
        assert_eq!(texts(&suggestions), ["Plumas", "Peso Pluma"]);
        assert!(suggestions[0].score > suggestions[1].score);
        assert!(complete(
            "upita",
            &[],
            &entities,
            &[],
            NOW,
            &CompletionConfig::default(),
            10
        )
        .is_empty());
        let start_only = CompletionConfig {
            inner_word_weight: 0.0,
            ..CompletionConfig::default()
        };
        assert_eq!(
            texts(&complete(
                "pluma",
                &[],
                &entities,
                &[],
                NOW,
                &start_only,
                10
            )),
            ["Plumas"]
        );
    }

    #[test]
    #[ignore = "timing: cargo test -p yt-search --release -- --ignored --nocapture"]
    fn completion_over_a_large_library_stays_fast() {
        use std::time::Instant;
        const NAMES: usize = 20_000;
        const HISTORY: usize = 300;
        const ROUNDS: usize = 200;
        let names: Vec<String> = (0..NAMES)
            .map(|index| match index % 3 {
                0 => format!("Bailando {index} (Remix)"),
                1 => format!("Canción número {index}"),
                _ => format!("Artist {}", index % 500),
            })
            .collect();
        let entities: Vec<&str> = names.iter().map(String::as_str).collect();
        let history: Vec<HistoryQuery> = (0..HISTORY)
            .map(|index| {
                past(
                    &format!("search {index}"),
                    1 + index as u32 % 7,
                    index as f64,
                )
            })
            .collect();
        let provider: Vec<String> = (0..10)
            .map(|index| format!("ba suggestion {index}"))
            .collect();
        let started = Instant::now();
        for _ in 0..ROUNDS {
            std::hint::black_box(complete(
                "ba",
                &history,
                &entities,
                &provider,
                NOW,
                &CompletionConfig::default(),
                8,
            ));
        }
        let mean = started.elapsed() / ROUNDS as u32;
        println!(
            "complete over {NAMES} names and {HISTORY} searches: mean {:.0} µs",
            mean.as_secs_f64() * 1e6
        );
    }

    #[test]
    fn equal_folds_merge_into_one_suggestion() {
        let history = [past("cancion", 1, 5.0), past("Canción", 2, 1.0)];
        let suggestions = history_only("can", &history, NOW);
        assert_eq!(texts(&suggestions), ["Canción"]);
        let invalid = CompletionConfig {
            history_half_life_days: -3.0,
            inner_word_weight: f64::INFINITY,
        };
        let sanitized = complete("can", &history, &[], &[], NOW, &invalid, 10);
        assert_eq!(sanitized, suggestions);
    }
}
