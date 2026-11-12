//! Diverse selection by maximal marginal relevance (Carbonell & Goldstein
//! 1998), and the song likeness that mixes and lists diversify on.

use crate::embedding::dot;

/// Who made a song and where it sits in the taste space: what mixes, weekly
/// lists and stations compare to keep variety.
pub(crate) struct SongTraits<'a> {
    /// The song's `artist_key`; empty when unknown.
    pub(crate) artist: String,
    /// Unit direction in the embedding space, when known.
    pub(crate) direction: Option<&'a [f64]>,
}

impl SongTraits<'_> {
    /// 1 for songs by one artist; otherwise the cosine of their directions
    /// clamped to [0, 1], or 0 when either direction is unknown.
    pub(crate) fn similarity(&self, other: &SongTraits<'_>) -> f64 {
        if !self.artist.is_empty() && self.artist == other.artist {
            return 1.0;
        }
        match (self.direction, other.direction) {
            (Some(left), Some(right)) => dot(left, right).clamp(0.0, 1.0),
            _ => 0.0,
        }
    }
}

/// The elements of `items` at the indices of `order`, in that order; an
/// index out of range or repeated is skipped.
pub(crate) fn take_in_order<T>(
    items: impl IntoIterator<Item = T>,
    order: impl IntoIterator<Item = usize>,
) -> Vec<T> {
    let mut slots: Vec<Option<T>> = items.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|index| slots.get_mut(index)?.take())
        .collect()
}

/// Greedily pick up to `limit` of `items`: each step takes the item with the
/// highest λ · relevance − (1 − λ) · (its largest similarity to the items
/// already picked).
///
/// `lambda` is clamped to [0, 1] (NaN counts as 1, pure relevance): 1 orders
/// by relevance alone, 0 by novelty alone. Similarities are expected in
/// [0, 1]; NaN similarities count as 0 and NaN relevance as the lowest
/// possible, so such items come last. Ties go to the earlier item, so equal
/// inputs give equal output. Costs O(n · limit) evaluations of each closure.
pub fn mmr<T: Clone>(
    items: &[T],
    relevance: impl Fn(&T) -> f64,
    similarity: impl Fn(&T, &T) -> f64,
    lambda: f64,
    limit: usize,
) -> Vec<T> {
    let lambda = if lambda.is_nan() {
        1.0
    } else {
        lambda.clamp(0.0, 1.0)
    };
    let relevance: Vec<f64> = items
        .iter()
        .map(|item| finite_or(relevance(item), f64::MIN))
        .collect();
    let mut redundancy = vec![0.0f64; items.len()];
    let mut picked = vec![false; items.len()];
    let mut selection = Vec::with_capacity(limit.min(items.len()));
    while selection.len() < limit {
        let mut best: Option<(usize, f64)> = None;
        for (index, value) in relevance.iter().enumerate() {
            if picked[index] {
                continue;
            }
            let score = lambda * value - (1.0 - lambda) * redundancy[index];
            if best.is_none_or(|(_, top)| score > top) {
                best = Some((index, score));
            }
        }
        let Some((chosen, _)) = best else {
            break;
        };
        picked[chosen] = true;
        selection.push(items[chosen].clone());
        for (index, item) in items.iter().enumerate() {
            if !picked[index] {
                let overlap = finite_or(similarity(item, &items[chosen]), 0.0);
                redundancy[index] = redundancy[index].max(overlap);
            }
        }
    }
    selection
}

/// `value` clamped to the finite range; NaN becomes `fallback`.
fn finite_or(value: f64, fallback: f64) -> f64 {
    if value.is_nan() {
        fallback
    } else {
        value.clamp(f64::MIN, f64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (id, artist, relevance)
    const ITEMS: [(&str, &str, f64); 6] = [
        ("x1", "X", 1.0),
        ("x2", "X", 0.95),
        ("x3", "X", 0.9),
        ("y1", "Y", 0.6),
        ("z1", "Z", 0.55),
        ("w1", "W", 0.5),
    ];

    fn pick(lambda: f64, limit: usize) -> Vec<&'static str> {
        mmr(
            &ITEMS,
            |item| item.2,
            |left, right| if left.1 == right.1 { 1.0 } else { 0.0 },
            lambda,
            limit,
        )
        .into_iter()
        .map(|item| item.0)
        .collect()
    }

    #[test]
    fn diversity_replaces_near_duplicates_with_other_relevant_items() {
        assert_eq!(pick(1.0, 3), ["x1", "x2", "x3"]);
        assert_eq!(pick(0.5, 3), ["x1", "y1", "z1"]);
        assert_eq!(pick(0.5, 10).len(), ITEMS.len());
        assert_eq!(pick(f64::NAN, 2), ["x1", "x2"]);
        assert!(pick(0.5, 0).is_empty());
    }

    #[test]
    fn invalid_scores_never_win() {
        let items = [f64::NAN, 0.2, 0.1];
        let order = mmr(&items, |value| *value, |_, _| f64::NAN, 0.7, 3);
        assert_eq!(order[..2], [0.2, 0.1]);
        assert!(order[2].is_nan());
    }

    #[test]
    fn songs_resemble_by_artist_then_by_taste_direction() {
        fn song<'a>(artist: &str, direction: Option<&'a [f64]>) -> SongTraits<'a> {
            SongTraits {
                artist: artist.into(),
                direction,
            }
        }
        let (east, north) = ([1.0, 0.0], [0.0, 1.0]);
        let diagonal = [0.6, 0.8];
        assert_eq!(song("a", None).similarity(&song("a", Some(&east))), 1.0);
        assert_eq!(song("", None).similarity(&song("", None)), 0.0);
        assert_eq!(
            song("a", Some(&east)).similarity(&song("b", Some(&north))),
            0.0
        );
        let close = song("a", Some(&diagonal)).similarity(&song("b", Some(&north)));
        assert!((close - 0.8).abs() < 1e-12);
        assert_eq!(song("a", Some(&east)).similarity(&song("b", None)), 0.0);
        assert_eq!(take_in_order(["x", "y", "z"], [2, 0, 2, 9]), ["z", "x"]);
    }
}
