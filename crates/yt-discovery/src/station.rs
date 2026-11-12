//! Station batches: continuity with the station's seeds, a bounded share of
//! exploration, and variety within each batch.

use crate::{
    artist_key,
    diversity::{mmr, take_in_order, SongTraits},
    embedding::{artist_vectors, centroid, dot},
    ranking::{admissible, base_score, provider_score, Distinct, SourcePresets},
    rng::stable_hash,
    title_key, ArtistBeliefs, Candidate, Embeddings, MediaAffinity, Profile, RankContext,
};
use std::collections::HashSet;

/// Largest share of a batch chosen to explore artists the user barely
/// knows; the rest follows the seeds.
const EXPLORATION_SHARE: f64 = 0.25;
/// Artists with less decayed listening evidence than this many listens are
/// unfamiliar, so exploration picks may try them.
const UNFAMILIAR_EVIDENCE: f64 = 2.0;
/// Weight of embedding similarity to the seeds, in ranking-score units.
const CONTINUITY_WEIGHT: f64 = 0.6;
/// MMR λ of a batch: relevance against similarity to songs already picked.
const STATION_LAMBDA: f64 = 0.75;

struct Pick<'a> {
    candidate: Candidate,
    traits: SongTraits<'a>,
    relevance: f64,
    exploration: Option<f64>,
    per_artist_cap: usize,
    tie: u64,
}

/// The next `limit` songs of a station steered by `seed_ids`, in play order.
///
/// Candidates pass the filters of their source's preset (exclusions,
/// lengths, repeated skips, recent listens): `RankingConfig::online` for
/// remote ids, `RankingConfig::library_autoplay` for library ids. They are
/// never a seed, a seed's song under another upload, or a repeated id or song.
/// Continuity relevance = the preset's `rank` score, with every seed's artist
/// (and `context.seed_artist`) earning the seed-artist bonus, + 0.6 × the
/// cosine between the candidate (its own vector, or its artist's direction)
/// and the seeds' embedding centroid. Every fourth position, at most a
/// quarter of the batch, is an exploration slot: it takes the unheard song of
/// an artist with under two decayed listens of evidence that has the best
/// Thompson sample plus provider order. Both orders are diversified by MMR
/// and every artist stays under its preset's cap.
///
/// Draws and tie-breaks derive from `rng_seed`; from `context`, `exclude`,
/// `seed_artist` and `now` are read and `profile` supplies affinities.
// The signature is the contract the application calls; bundling these
// inputs into a struct would only rename them.
#[allow(clippy::too_many_arguments)]
pub fn station_picks(
    candidates: Vec<Candidate>,
    profile: &Profile,
    beliefs: &ArtistBeliefs,
    embeddings: &Embeddings,
    seed_ids: &[String],
    context: &RankContext<'_>,
    rng_seed: u64,
    limit: usize,
) -> Vec<Candidate> {
    let presets = SourcePresets::new();
    let seeds: Vec<&MediaAffinity> = seed_ids
        .iter()
        .filter_map(|id| profile.media_affinity(id))
        .collect();
    let mut seed_artists: Vec<String> = seeds
        .iter()
        .map(|seed| artist_key(&seed.artist))
        .chain(context.seed_artist.map(artist_key))
        .filter(|key| !key.is_empty())
        .collect();
    seed_artists.sort();
    seed_artists.dedup();
    let seed_songs: HashSet<String> = seeds
        .iter()
        .map(|seed| title_key(&seed.title))
        .filter(|key| !key.is_empty())
        .collect();
    let seed_set: HashSet<&str> = seed_ids.iter().map(String::as_str).collect();
    let anchor = centroid(
        seed_ids.iter().filter_map(|id| embeddings.vector(id)),
        embeddings.dimensions(),
    );
    let directions = artist_vectors(profile, embeddings);
    let mut pool: Vec<Pick<'_>> = Vec::new();
    for candidate in candidates {
        let preset = presets.for_media(&candidate.media_id);
        if seed_set.contains(candidate.media_id.as_str())
            || seed_songs.contains(&title_key(&candidate.title))
            || !admissible(&candidate, profile, context.exclude, context.now, preset)
        {
            continue;
        }
        let artist = artist_key(&candidate.artist);
        let direction = embeddings
            .vector(&candidate.media_id)
            .or_else(|| directions.get(&artist).map(Vec::as_slice));
        let continuity = anchor
            .as_deref()
            .zip(direction)
            .map_or(0.0, |(anchor, direction)| dot(anchor, direction));
        let relevance =
            base_score(&candidate, profile, preset, &seed_artists) + CONTINUITY_WEIGHT * continuity;
        let unfamiliar = !seed_artists.contains(&artist)
            && profile.media_affinity(&candidate.media_id).is_none()
            && beliefs.evidence(&candidate.artist) < UNFAMILIAR_EVIDENCE;
        let exploration = unfamiliar.then(|| {
            beliefs.sample_keyed(&candidate.artist, rng_seed)
                + provider_score(candidate.provider_rank, preset.provider_decay)
        });
        pool.push(Pick {
            tie: stable_hash(rng_seed, &candidate.media_id),
            per_artist_cap: preset.per_artist_cap,
            traits: SongTraits { artist, direction },
            relevance,
            exploration,
            candidate,
        });
    }
    pool.sort_by(|left, right| {
        right
            .relevance
            .total_cmp(&left.relevance)
            .then(left.tie.cmp(&right.tie))
    });
    let everyone: Vec<usize> = (0..pool.len()).collect();
    let similarity =
        |left: &usize, right: &usize| pool[*left].traits.similarity(&pool[*right].traits);
    let follow = mmr(
        &everyone,
        |index| pool[*index].relevance,
        similarity,
        STATION_LAMBDA,
        pool.len(),
    );
    let mut explorers: Vec<usize> = everyone
        .into_iter()
        .filter(|index| pool[*index].exploration.is_some())
        .collect();
    let exploration_of = |index: usize| pool[index].exploration.unwrap_or(f64::MIN);
    explorers.sort_by(|left, right| {
        exploration_of(*right)
            .total_cmp(&exploration_of(*left))
            .then(pool[*left].tie.cmp(&pool[*right].tie))
    });
    let explore = mmr(
        &explorers,
        |index| exploration_of(*index),
        similarity,
        STATION_LAMBDA,
        explorers.len(),
    );
    let slots = (limit as f64 * EXPLORATION_SHARE).floor() as usize;
    let (mut follow, mut explore) = (follow.into_iter(), explore.into_iter());
    let mut distinct = Distinct::default();
    let mut picks = Vec::with_capacity(limit.min(pool.len()));
    let mut explored = 0;
    while picks.len() < limit {
        let exploring = explored < slots && is_exploration_slot(picks.len());
        let next = if exploring {
            next_admitted(&mut explore, &pool, &mut distinct)
                .inspect(|_| explored += 1)
                .or_else(|| next_admitted(&mut follow, &pool, &mut distinct))
        } else {
            next_admitted(&mut follow, &pool, &mut distinct)
        };
        let Some(index) = next else {
            break;
        };
        picks.push(index);
    }
    take_in_order(pool.into_iter().map(|pick| pick.candidate), picks)
}

/// The next index of `order` that `distinct` admits under its artist's cap.
fn next_admitted(
    order: &mut impl Iterator<Item = usize>,
    pool: &[Pick<'_>],
    distinct: &mut Distinct,
) -> Option<usize> {
    order.find(|index| distinct.admit(&pool[*index].candidate, pool[*index].per_artist_cap))
}

/// Whether batch `position` is an exploration slot: the last of every run
/// of 1 / `EXPLORATION_SHARE` positions.
fn is_exploration_slot(position: usize) -> bool {
    ((position + 1) as f64 * EXPLORATION_SHARE).floor()
        > (position as f64 * EXPLORATION_SHARE).floor()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        sessions,
        test_support::{candidate, group, grouped_history, DAY, NOW},
        AffinityConfig, EmbeddingConfig, ExploreConfig, ListenPolicy, SESSION_GAP_SECONDS,
    };

    struct World {
        profile: Profile,
        beliefs: ArtistBeliefs,
        embeddings: Embeddings,
    }

    /// «s0» (by «Seed Artist») shares sessions with «s1»…«s7», each by its own
    /// artist; «r0»…«r7» are heard as often but never with them.
    fn world() -> World {
        let groups = [
            group("s", 8, |index| {
                if index == 0 {
                    "Seed Artist".to_owned()
                } else {
                    format!("Echo {index}")
                }
            }),
            group("r", 8, |index| format!("Rest {index}")),
        ];
        let signals = grouped_history(&groups, 6, NOW - 2.0 * DAY, 4);
        let sessions = sessions(&signals, SESSION_GAP_SECONDS, &ListenPolicy::default());
        World {
            profile: Profile::build(&signals, &[], NOW, &AffinityConfig::default()),
            beliefs: ArtistBeliefs::build(&signals, NOW, &ExploreConfig::default()),
            embeddings: Embeddings::build(&sessions, &EmbeddingConfig::default(), 2),
        }
    }

    fn picks(world: &World, exclude: &HashSet<String>, seed: u64, limit: usize) -> Vec<Candidate> {
        let mut copy = candidate("s0-copy", "Seed Artist - Topic", 0);
        copy.title = "Song s0 (Official Audio)".into();
        let mut list = vec![
            candidate("s0", "Seed Artist", 0),
            copy,
            candidate("seed-artist-new", "Seed Artist", 0),
            candidate("excluded", "Echo 1", 0),
        ];
        list.extend(
            (1..4).map(|index| candidate(&format!("s{index}"), &format!("Echo {index}"), 0)),
        );
        list.extend(
            (1..4).map(|index| candidate(&format!("r{index}"), &format!("Rest {index}"), 0)),
        );
        list.extend(
            (0..4).map(|index| candidate(&format!("new{index}"), &format!("Stranger {index}"), 0)),
        );
        let context = RankContext {
            profile: &world.profile,
            exclude,
            seed_artist: None,
            now: NOW,
            salt: 0,
        };
        station_picks(
            list,
            &world.profile,
            &world.beliefs,
            &world.embeddings,
            &["s0".to_owned()],
            &context,
            seed,
            limit,
        )
    }

    #[test]
    fn stations_follow_their_seeds_and_never_repeat_or_break_exclusions() {
        let world = world();
        let exclude = HashSet::from(["excluded".to_owned()]);
        let batch = picks(&world, &exclude, 7, 8);
        let ids: Vec<&str> = batch.iter().map(|pick| pick.media_id.as_str()).collect();
        assert_eq!(ids.len(), 8);
        for forbidden in ["s0", "s0-copy", "excluded"] {
            assert!(!ids.contains(&forbidden), "{forbidden} picked: {ids:?}");
        }
        assert_eq!(ids[0], "seed-artist-new");
        let position = |id: &str| ids.iter().position(|picked| *picked == id);
        let similar: Vec<usize> = ["s1", "s2", "s3"]
            .iter()
            .filter_map(|id| position(id))
            .collect();
        let unrelated: Vec<usize> = ["r1", "r2", "r3"]
            .iter()
            .filter_map(|id| position(id))
            .collect();
        assert_eq!(similar.len(), 3);
        assert!(
            similar.iter().all(|s| unrelated.iter().all(|r| s < r)),
            "songs heard with the seed come first: {ids:?}"
        );
        let strangers = ids.iter().filter(|id| id.starts_with("new")).count();
        assert!(
            (1..=2).contains(&strangers),
            "explores a bounded share: {ids:?}"
        );
        let unique: HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len());
    }

    #[test]
    fn a_seed_fixes_the_batch() {
        let world = world();
        let exclude = HashSet::new();
        assert_eq!(picks(&world, &exclude, 3, 8), picks(&world, &exclude, 3, 8));
        assert!(picks(&world, &exclude, 3, 0).is_empty());
        // Every candidate but the seed and its other upload.
        assert_eq!(picks(&world, &exclude, 3, 100).len(), 12);
    }
}
