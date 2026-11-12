//! Taste clusters: groups of liked songs that sit together in the user's own
//! listening, so each side of their taste can get its own mix.

use crate::{artist_key, rng::SeededRng, Embeddings, MediaAffinity, Profile};
use std::collections::HashMap;

/// Artist names shown per cluster.
const CLUSTER_ARTISTS: usize = 3;
/// Base of the 2 + ⌊ln k⌋ candidates greedy k-means++ draws per seeding
/// step, keeping the one that lowers the potential most; it avoids most bad
/// seedings of plain k-means++.
const GREEDY_SEEDING_BASE_TRIALS: usize = 2;

/// One side of the user's taste.
#[derive(Debug, Clone, PartialEq)]
pub struct TasteCluster {
    /// Liked media of the cluster by descending affinity.
    pub media_ids: Vec<String>,
    /// Display names of its up to three most liked artists.
    pub artists: Vec<String>,
    /// Summed affinity of its media.
    pub weight: f64,
}

/// How liked media are grouped.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterConfig {
    /// Most clusters returned.
    pub max_clusters: usize,
    /// Clusters with fewer media are dropped; the target size of a cluster
    /// when choosing how many to look for.
    pub min_cluster_size: usize,
    /// Most Lloyd iterations of k-means; it stops earlier once assignments
    /// settle.
    pub iterations: usize,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            max_clusters: 6,
            min_cluster_size: 4,
            iterations: 25,
        }
    }
}

/// The user's taste clusters, heaviest first, deterministic for a `seed`.
///
/// Liked media (positive affinity) with an embedding are grouped by k-means
/// on their unit vectors, each weighted by its affinity, seeded by greedy
/// k-means++ (Arthur & Vassilvitskii 2007). The number of clusters is
/// √(n / 2) for n embedded liked media (Mardia, Kent & Bibby 1979), at most
/// `max_clusters` and at most n / `min_cluster_size`. Liked media without a
/// vector join the cluster holding most of their artist's affinity. When
/// fewer than `min_cluster_size` liked media are embedded (for example with
/// empty embeddings), liked media group by artist instead. Clusters smaller
/// than `min_cluster_size` are dropped; ties break by media ids.
pub fn taste_clusters(
    profile: &Profile,
    embeddings: &Embeddings,
    config: &ClusterConfig,
    seed: u64,
) -> Vec<TasteCluster> {
    if config.max_clusters == 0 {
        return Vec::new();
    }
    let min_size = config.min_cluster_size.max(1);
    let liked: Vec<&MediaAffinity> = profile
        .media()
        .iter()
        .filter(|item| item.score > 0.0)
        .collect();
    let embedded: Vec<(usize, &[f64])> = liked
        .iter()
        .enumerate()
        .filter_map(|(index, item)| Some((index, embeddings.vector(&item.media_id)?)))
        .collect();
    if embedded.len() < min_size {
        return by_artist(&liked, config.max_clusters, min_size);
    }
    let points: Vec<&[f64]> = embedded.iter().map(|(_, vector)| *vector).collect();
    let weights: Vec<f64> = embedded
        .iter()
        .map(|(index, _)| liked[*index].score)
        .collect();
    let count = cluster_count(points.len(), config.max_clusters, min_size);
    let assignment = kmeans(
        &points,
        &weights,
        count,
        config.iterations,
        &mut SeededRng::new(seed),
    );
    let mut membership: Vec<Option<usize>> = vec![None; liked.len()];
    for ((index, _), cluster) in embedded.iter().zip(&assignment) {
        membership[*index] = Some(*cluster);
    }
    let homes = artist_homes(&liked, &membership, count);
    for (item, cluster) in liked.iter().zip(membership.iter_mut()) {
        if cluster.is_none() {
            *cluster = homes.get(&artist_key(&item.artist)).copied();
        }
    }
    let mut groups: Vec<Vec<&MediaAffinity>> = vec![Vec::new(); count];
    for (item, cluster) in liked.iter().zip(&membership) {
        if let Some(cluster) = cluster {
            groups[*cluster].push(item);
        }
    }
    finish(groups, config.max_clusters, min_size)
}

/// k ≈ √(n / 2), at most `max_clusters` and `points` / `min_size`, at least 1.
fn cluster_count(points: usize, max_clusters: usize, min_size: usize) -> usize {
    let rule = (points as f64 / 2.0).sqrt().round() as usize;
    rule.min(max_clusters).min(points / min_size).max(1)
}

/// For every artist key, the cluster holding most of its embedded affinity.
fn artist_homes(
    liked: &[&MediaAffinity],
    membership: &[Option<usize>],
    count: usize,
) -> HashMap<String, usize> {
    let mut mass: HashMap<String, Vec<f64>> = HashMap::new();
    for (item, cluster) in liked.iter().zip(membership) {
        let Some(cluster) = cluster else {
            continue;
        };
        let key = artist_key(&item.artist);
        if !key.is_empty() {
            mass.entry(key).or_insert_with(|| vec![0.0; count])[*cluster] += item.score;
        }
    }
    mass.into_iter()
        .filter_map(|(key, clusters)| {
            let (home, weight) =
                clusters
                    .iter()
                    .enumerate()
                    .fold((0, 0.0), |best, (cluster, weight)| {
                        if *weight > best.1 {
                            (cluster, *weight)
                        } else {
                            best
                        }
                    });
            (weight > 0.0).then_some((key, home))
        })
        .collect()
}

/// Liked media grouped by artist key, for profiles without embeddings.
fn by_artist(liked: &[&MediaAffinity], max_clusters: usize, min_size: usize) -> Vec<TasteCluster> {
    let mut groups: Vec<Vec<&MediaAffinity>> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();
    for item in liked {
        let key = artist_key(&item.artist);
        if key.is_empty() {
            continue;
        }
        let position = *positions.entry(key).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[position].push(item);
    }
    finish(groups, max_clusters, min_size)
}

/// Clusters from groups of media in profile order: small ones dropped,
/// heaviest first, at most `max_clusters`.
fn finish(
    groups: Vec<Vec<&MediaAffinity>>,
    max_clusters: usize,
    min_size: usize,
) -> Vec<TasteCluster> {
    let mut clusters: Vec<TasteCluster> = groups
        .into_iter()
        .filter(|members| members.len() >= min_size)
        .map(|members| cluster(&members))
        .collect();
    clusters.sort_by(|left, right| {
        right
            .weight
            .total_cmp(&left.weight)
            .then_with(|| left.media_ids.cmp(&right.media_ids))
    });
    clusters.truncate(max_clusters);
    clusters
}

fn cluster(members: &[&MediaAffinity]) -> TasteCluster {
    let mut artists: Vec<(String, String, f64)> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();
    for item in members {
        let key = artist_key(&item.artist);
        if key.is_empty() {
            continue;
        }
        let position = *positions.entry(key.clone()).or_insert_with(|| {
            artists.push((key, item.artist.clone(), 0.0));
            artists.len() - 1
        });
        artists[position].2 += item.score;
    }
    artists.sort_by(|left, right| {
        right
            .2
            .total_cmp(&left.2)
            .then_with(|| left.0.cmp(&right.0))
    });
    TasteCluster {
        media_ids: members.iter().map(|item| item.media_id.clone()).collect(),
        artists: artists
            .into_iter()
            .take(CLUSTER_ARTISTS)
            .map(|(_, name, _)| name)
            .collect(),
        weight: members.iter().map(|item| item.score).sum(),
    }
}

/// Weighted k-means of `points` into at most `count` clusters; returns each
/// point's cluster.
fn kmeans(
    points: &[&[f64]],
    weights: &[f64],
    count: usize,
    iterations: usize,
    rng: &mut SeededRng,
) -> Vec<usize> {
    let mut centers = seed_centers(points, weights, count, rng);
    let mut assignment: Vec<usize> = points
        .iter()
        .map(|point| nearest_center(point, &centers))
        .collect();
    for _ in 0..iterations {
        let dimensions = centers[0].len();
        let mut sums = vec![vec![0.0; dimensions]; centers.len()];
        let mut mass = vec![0.0; centers.len()];
        for ((point, weight), cluster) in points.iter().zip(weights).zip(&assignment) {
            for (sum, value) in sums[*cluster].iter_mut().zip(point.iter()) {
                *sum += weight * value;
            }
            mass[*cluster] += weight;
        }
        for ((center, sum), total) in centers.iter_mut().zip(sums).zip(mass) {
            // An emptied cluster keeps its center; it may win points back.
            if total > 0.0 {
                *center = sum.into_iter().map(|value| value / total).collect();
            }
        }
        let next: Vec<usize> = points
            .iter()
            .map(|point| nearest_center(point, &centers))
            .collect();
        if next == assignment {
            break;
        }
        assignment = next;
    }
    assignment
}

/// Greedy weighted k-means++: the first center by weight, every next one
/// among 2 + ⌊ln k⌋ candidates drawn by weight × squared distance to the
/// nearest center, keeping the candidate that lowers the total most. Stops
/// early when every point coincides with a center.
fn seed_centers(
    points: &[&[f64]],
    weights: &[f64],
    count: usize,
    rng: &mut SeededRng,
) -> Vec<Vec<f64>> {
    let trials = GREEDY_SEEDING_BASE_TRIALS + (count as f64).ln().floor() as usize;
    let first = points[weighted_index(weights, rng)].to_vec();
    let mut nearest: Vec<f64> = points
        .iter()
        .map(|point| squared_distance(point, &first))
        .collect();
    let mut centers = vec![first];
    while centers.len() < count {
        let scores: Vec<f64> = weights
            .iter()
            .zip(&nearest)
            .map(|(weight, distance)| weight * distance)
            .collect();
        if scores.iter().sum::<f64>() <= 0.0 {
            break;
        }
        let mut best: Option<(f64, usize, Vec<f64>)> = None;
        for _ in 0..trials {
            let candidate = weighted_index(&scores, rng);
            let distances: Vec<f64> = points
                .iter()
                .zip(&nearest)
                .map(|(point, current)| current.min(squared_distance(point, points[candidate])))
                .collect();
            let potential: f64 = distances
                .iter()
                .zip(weights)
                .map(|(distance, weight)| distance * weight)
                .sum();
            if best
                .as_ref()
                .is_none_or(|(lowest, _, _)| potential < *lowest)
            {
                best = Some((potential, candidate, distances));
            }
        }
        let Some((_, chosen, distances)) = best else {
            break;
        };
        nearest = distances;
        centers.push(points[chosen].to_vec());
    }
    centers
}

/// An index drawn with probability proportional to its non-negative weight.
fn weighted_index(weights: &[f64], rng: &mut SeededRng) -> usize {
    let total: f64 = weights.iter().sum();
    let mut target = rng.next_f64() * total;
    for (index, weight) in weights.iter().enumerate() {
        if target < *weight {
            return index;
        }
        target -= weight;
    }
    // Rounding can leave the target past the last weight.
    weights
        .iter()
        .rposition(|weight| *weight > 0.0)
        .unwrap_or(0)
}

fn nearest_center(point: &[f64], centers: &[Vec<f64>]) -> usize {
    centers
        .iter()
        .enumerate()
        .fold((0, f64::INFINITY), |best, (index, center)| {
            let distance = squared_distance(point, center);
            if distance < best.1 {
                (index, distance)
            } else {
                best
            }
        })
        .0
}

fn squared_distance(left: &[f64], right: &[f64]) -> f64 {
    left.iter().zip(right).map(|(a, b)| (a - b) * (a - b)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        sessions,
        test_support::{completed, group, grouped_history, DAY, HOUR, NOW},
        AffinityConfig, EmbeddingConfig, FavoriteSignal, ListenPolicy, SESSION_GAP_SECONDS,
    };
    use std::time::{Duration, Instant};

    fn two_groups() -> (Profile, Embeddings) {
        // Artists are shared across groups, so only listening context can
        // separate them; «Solo» only appears in the first group.
        let groups = [
            group("a", 10, |index| {
                if index == 0 {
                    "Solo".to_owned()
                } else {
                    format!("Artist {}", index % 3)
                }
            }),
            group("b", 10, |index| format!("Artist {}", index % 3)),
        ];
        let signals = grouped_history(&groups, 8, NOW - DAY, 11);
        let sessions = sessions(&signals, SESSION_GAP_SECONDS, &ListenPolicy::default());
        let favorite = FavoriteSignal {
            media_id: "solo-favorite".into(),
            title: "Favorite".into(),
            artist: "Solo".into(),
            remote: false,
            added_at: NOW - DAY,
        };
        (
            Profile::build(&signals, &[favorite], NOW, &AffinityConfig::default()),
            Embeddings::build(&sessions, &EmbeddingConfig::default(), 5),
        )
    }

    #[test]
    fn clusters_separate_listening_groups_even_across_shared_artists() {
        let (profile, embeddings) = two_groups();
        let clusters = taste_clusters(&profile, &embeddings, &ClusterConfig::default(), 1);
        assert!(clusters.len() >= 2);
        let mut groups_seen = Vec::new();
        for cluster in &clusters {
            let first = cluster.media_ids[0].chars().next().unwrap();
            let pure = cluster
                .media_ids
                .iter()
                .filter(|id| id.as_str() != "solo-favorite")
                .all(|id| id.starts_with(first));
            assert!(pure, "mixed cluster {:?}", cluster.media_ids);
            assert!(cluster.media_ids.len() >= 4 && cluster.artists.len() <= 3);
            groups_seen.push(first);
        }
        assert!(groups_seen.contains(&'a') && groups_seen.contains(&'b'));
        assert!(clusters
            .windows(2)
            .all(|pair| pair[0].weight >= pair[1].weight));
        let home = clusters
            .iter()
            .find(|cluster| cluster.media_ids.contains(&"solo-favorite".to_owned()))
            .expect("the favorite joins its artist's cluster");
        assert!(home.media_ids.contains(&"a0".to_owned()));
        assert_eq!(
            taste_clusters(&profile, &embeddings, &ClusterConfig::default(), 1),
            clusters
        );
    }

    #[test]
    fn without_embeddings_clusters_group_by_artist() {
        let mut signals = Vec::new();
        for (artist, songs) in [("Big", 5), ("Mid", 4), ("Small", 2)] {
            for song in 0..songs {
                signals.push(completed(&format!("{artist}{song}"), artist, NOW - HOUR));
            }
        }
        let profile = Profile::build(&signals, &[], NOW, &AffinityConfig::default());
        let clusters = taste_clusters(
            &profile,
            &Embeddings::default(),
            &ClusterConfig::default(),
            1,
        );
        let artists: Vec<&[String]> = clusters
            .iter()
            .map(|cluster| cluster.artists.as_slice())
            .collect();
        assert_eq!(artists, [["Big".to_owned()], ["Mid".to_owned()]]);
        assert_eq!(clusters[0].media_ids.len(), 5);
        let none = ClusterConfig {
            max_clusters: 0,
            ..ClusterConfig::default()
        };
        assert!(taste_clusters(&profile, &Embeddings::default(), &none, 1).is_empty());
    }

    #[test]
    #[ignore = "timing; run with `cargo test --release -p yt-discovery -- --ignored --nocapture`"]
    fn a_full_history_embeds_and_clusters_in_well_under_100_ms() {
        const ITEMS: usize = 1_500;
        const EVENTS: usize = 5_000;
        const SCENES: usize = 30;
        const SESSION_LENGTH: usize = 20;
        const IN_SCENE: f64 = 0.85;
        // Coprime with the songs per scene, so a scene's cursor visits all of
        // them in a scrambled order and every song is heard at least twice.
        const SCENE_STRIDE: usize = 7;
        let mut rng = SeededRng::new(2026);
        let per_scene = ITEMS / SCENES;
        let mut cursors = vec![0; SCENES];
        let mut signals = Vec::with_capacity(EVENTS);
        let mut clock = NOW - 60.0 * DAY;
        for session in 0.. {
            if signals.len() >= EVENTS {
                break;
            }
            let scene = session % SCENES;
            for _ in 0..SESSION_LENGTH.min(EVENTS - signals.len()) {
                let item = if rng.next_f64() < IN_SCENE {
                    cursors[scene] = (cursors[scene] + SCENE_STRIDE) % per_scene;
                    scene * per_scene + cursors[scene]
                } else {
                    (rng.next_f64() * ITEMS as f64) as usize
                };
                signals.push(completed(
                    &format!("m{item}"),
                    &format!("Artist {}", item / 5),
                    clock,
                ));
                clock += 200.0;
            }
            clock += 3.0 * HOUR;
        }
        let profile = Profile::build(&signals, &[], NOW, &AffinityConfig::default());
        let started = Instant::now();
        let sessions = sessions(&signals, SESSION_GAP_SECONDS, &ListenPolicy::default());
        let embeddings = Embeddings::build(&sessions, &EmbeddingConfig::default(), 1);
        let embedded = started.elapsed();
        let clusters = taste_clusters(&profile, &embeddings, &ClusterConfig::default(), 1);
        let total = started.elapsed();
        println!(
            "{EVENTS} events, {} distinct songs: {} embedded in {embedded:?}; {} clusters, {total:?} in all",
            profile.media().len(),
            embeddings.len(),
            clusters.len(),
        );
        assert_eq!(profile.media().len(), ITEMS);
        assert!(embeddings.len() >= ITEMS * 95 / 100);
        assert_eq!(clusters.len(), ClusterConfig::default().max_clusters);
        assert!(total < Duration::from_millis(100), "took {total:?}");
    }
}
