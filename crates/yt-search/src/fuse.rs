//! Merging ranked lists from different sources, and re-ranking by what the
//! user likes.

use std::collections::{HashMap, HashSet};

/// Rank constant of reciprocal rank fusion. Cormack, Clarke & Büttcher (2009)
/// found 60 near-optimal across collections: it keeps one list's top items
/// from dominating while still rewarding agreement.
pub const DEFAULT_RRF_K: f64 = 60.0;

/// Reciprocal rank fusion (Cormack, Clarke & Büttcher 2009) of ranked id
/// lists, best first: each id scores Σ 1 / (k + rank) over the lists that
/// contain it, with ranks counted from 1 among a list's distinct ids (a
/// repeated id counts once per list, at its best rank).
///
/// Only ranks matter, so lists from incomparable scorers (library BM25,
/// online provider order) merge fairly. Equal scores break by the best rank
/// an id reached, then by the earlier list reaching it: fusing `[a, b]` with
/// `[x, y]` gives `a, x, b, y`. A negative or non-finite `k` falls back to
/// [`DEFAULT_RRF_K`]. Time O(total ids · log).
pub fn reciprocal_rank_fusion(lists: &[Vec<String>], k: f64) -> Vec<(String, f64)> {
    let k = if k.is_finite() && k >= 0.0 {
        k
    } else {
        DEFAULT_RRF_K
    };
    let mut fused: Vec<Fused<'_>> = Vec::new();
    let mut positions: HashMap<&str, usize> = HashMap::new();
    for (list, ids) in lists.iter().enumerate() {
        let mut seen = HashSet::new();
        for id in ids {
            if !seen.insert(id.as_str()) {
                continue;
            }
            let rank = seen.len();
            let contribution = 1.0 / (k + rank as f64);
            match positions.get(id.as_str()) {
                Some(position) => {
                    let entry = &mut fused[*position];
                    entry.score += contribution;
                    entry.best = entry.best.min((rank, list));
                }
                None => {
                    positions.insert(id.as_str(), fused.len());
                    fused.push(Fused {
                        id: id.as_str(),
                        score: contribution,
                        best: (rank, list),
                    });
                }
            }
        }
    }
    // `best` differs between ids (one id per rank of a list): a total order.
    fused.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then(left.best.cmp(&right.best))
    });
    fused
        .into_iter()
        .map(|entry| (entry.id.to_owned(), entry.score))
        .collect()
}

/// Re-ranks `scored` (id, score) pairs by the user's affinity, in the spirit
/// of Teevan, Dumais & Horvitz (2005): each score gains
/// `weight · tanh(affinity(id))`, a boost bounded by ±`weight`, so liked ids
/// rise past close competitors and disliked ones (negative affinity) sink,
/// but an id never passes one that scored `2 · weight` more (`weight` more
/// when that one is neutral): large relevance gaps survive.
///
/// `weight` is in the units of the scores (with [`reciprocal_rank_fusion`]
/// scores, a fraction of 1 / (k + 1)); a negative or non-finite weight, or a
/// NaN affinity, means no boost, and an infinite affinity gives the full
/// ±`weight`. Returns the pairs with boosted scores, best first; ties keep
/// their input order.
pub fn personalize(
    scored: Vec<(String, f64)>,
    affinity: impl Fn(&str) -> f64,
    weight: f64,
) -> Vec<(String, f64)> {
    let weight = if weight.is_finite() {
        weight.max(0.0)
    } else {
        0.0
    };
    let mut boosted: Vec<(String, f64)> = scored
        .into_iter()
        .map(|(id, score)| {
            let liking = affinity(&id);
            let boost = if liking.is_nan() {
                0.0
            } else {
                weight * liking.tanh()
            };
            (id, score + boost)
        })
        .collect();
    // Stable, so equal scores keep the caller's order.
    boosted.sort_by(|left, right| right.1.total_cmp(&left.1));
    boosted
}

struct Fused<'a> {
    id: &'a str,
    score: f64,
    /// Best (rank, list index) the id reached.
    best: (usize, usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lists(lists: &[&[&str]]) -> Vec<Vec<String>> {
        lists
            .iter()
            .map(|list| list.iter().map(|id| (*id).to_owned()).collect())
            .collect()
    }

    fn ids(scored: &[(String, f64)]) -> Vec<&str> {
        scored.iter().map(|(id, _)| id.as_str()).collect()
    }

    fn score(scored: &[(String, f64)], id: &str) -> f64 {
        scored
            .iter()
            .find(|(candidate, _)| candidate == id)
            .map_or(f64::NAN, |(_, score)| *score)
    }

    #[test]
    fn fusion_rewards_agreement_between_lists() {
        let fused = reciprocal_rank_fusion(
            &lists(&[&["a", "b", "c"], &["b", "c", "a"], &["c", "b"]]),
            60.0,
        );
        assert_eq!(ids(&fused), ["b", "c", "a"]);
        let expected = [
            ("a", 1.0 / 61.0 + 1.0 / 63.0),
            ("b", 1.0 / 62.0 + 1.0 / 61.0 + 1.0 / 62.0),
            ("c", 1.0 / 63.0 + 1.0 / 62.0 + 1.0 / 61.0),
        ];
        for (id, value) in expected {
            assert!((score(&fused, id) - value).abs() < 1e-12, "{id}");
        }
    }

    #[test]
    fn fusion_ties_interleave_lists_in_order() {
        let fused = reciprocal_rank_fusion(&lists(&[&["a", "b"], &["x", "y"]]), DEFAULT_RRF_K);
        assert_eq!(ids(&fused), ["a", "x", "b", "y"]);
        let fused = reciprocal_rank_fusion(&lists(&[&["x", "y"], &["a", "b"]]), DEFAULT_RRF_K);
        assert_eq!(ids(&fused), ["x", "a", "y", "b"]);
    }

    #[test]
    fn fusion_counts_repeats_once_and_sanitizes_k() {
        let fused = reciprocal_rank_fusion(&lists(&[&["a", "a", "b"]]), 60.0);
        assert_eq!(ids(&fused), ["a", "b"]);
        assert!((score(&fused, "b") - 1.0 / 62.0).abs() < 1e-12);
        let input = lists(&[&["a", "b"], &["b"]]);
        let default = reciprocal_rank_fusion(&input, DEFAULT_RRF_K);
        assert_eq!(reciprocal_rank_fusion(&input, -1.0), default);
        assert_eq!(reciprocal_rank_fusion(&input, f64::NAN), default);
        assert!(reciprocal_rank_fusion(&[], 60.0).is_empty());
        let unit = reciprocal_rank_fusion(&input, 0.0);
        assert!((score(&unit, "b") - 1.5).abs() < 1e-12);
    }

    #[test]
    fn personalization_lifts_liked_items_without_overturning_large_gaps() {
        let scored = vec![
            ("a".to_owned(), 1.0),
            ("b".to_owned(), 0.98),
            ("c".to_owned(), 0.5),
        ];
        let affinity = |id: &str| match id {
            "b" => 2.0,
            "c" => 50.0,
            _ => 0.0,
        };
        let boosted = personalize(scored.clone(), affinity, 0.05);
        assert_eq!(ids(&boosted), ["b", "a", "c"]);
        assert!(score(&boosted, "c") <= 0.5 + 0.05);
        let disliked = personalize(
            scored.clone(),
            |id| if id == "a" { -3.0 } else { 0.0 },
            0.05,
        );
        assert_eq!(ids(&disliked), ["b", "a", "c"]);
    }

    #[test]
    fn personalization_without_weight_keeps_the_order() {
        let scored = vec![
            ("a".to_owned(), 1.0),
            ("b".to_owned(), 1.0),
            ("c".to_owned(), 0.5),
        ];
        for weight in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                personalize(scored.clone(), |_| 10.0, weight),
                scored,
                "{weight}"
            );
        }
        assert_eq!(personalize(scored.clone(), |_| f64::NAN, 1.0), scored);
        assert_eq!(ids(&personalize(scored, |_| 1.0, 0.1)), ["a", "b", "c"]);
    }
}
