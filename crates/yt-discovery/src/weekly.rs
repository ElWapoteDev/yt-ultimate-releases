//! «Descubrimiento semanal»: songs the user never heard, chosen once per
//! week from the related media the application fetched.
//!
//! Relevance follows the provider's order and the user's artist affinity;
//! exploration samples each artist's Beta posterior (Thompson sampling,
//! Chapelle & Li 2011; the bandit view of music recommendation of
//! McInerney et al. 2018), so unknown artists draw from the prior and earn a
//! place by chance; a novelty bonus and a cap on known artists keep the list
//! a discovery; maximal marginal relevance spreads it across the taste space.

use crate::{
    artist_key,
    diversity::{mmr, take_in_order, SongTraits},
    embedding::artist_vectors,
    ranking::{admissible, base_score, Distinct, SourcePresets},
    rng::stable_hash,
    title_key, ArtistBeliefs, Candidate, Embeddings, Profile, RankContext,
};
use std::collections::HashSet;

/// Shape of the weekly list.
#[derive(Debug, Clone, PartialEq)]
pub struct WeeklyConfig {
    /// Most songs in the list.
    pub limit: usize,
    /// Most songs per artist.
    pub per_artist_cap: usize,
    /// Share of `limit`, in [0, 1], that may come from artists the user
    /// already heard; the rest introduces new artists. Known artists fill
    /// past it only when new artists run out.
    pub known_artist_share: f64,
    /// MMR λ in [0, 1]: 1 orders by relevance alone, 0 by diversity alone.
    pub mmr_lambda: f64,
    /// Weight of the artist's Thompson sample, in relevance units.
    pub exploration_weight: f64,
    /// Bonus for artists the user never heard, in relevance units.
    pub novelty_weight: f64,
}

impl Default for WeeklyConfig {
    fn default() -> Self {
        Self {
            limit: 30,
            per_artist_cap: 1,
            known_artist_share: 0.3,
            mmr_lambda: 0.7,
            exploration_weight: 0.5,
            novelty_weight: 0.2,
        }
    }
}

struct Pick<'a> {
    candidate: Candidate,
    traits: SongTraits<'a>,
    known: bool,
    relevance: f64,
    tie: u64,
}

/// This week's discoveries from `candidates`, best first.
///
/// Only media the user never heard nor favorited take part, never a song the
/// profile already holds under another upload (same `artist_key` and
/// `title_key`), never an id in `context.exclude`, and only lengths the
/// source's ranking preset accepts (music lengths online, any length from
/// the library). Relevance = provider order + artist affinity (as `rank`
/// scores them) + `exploration_weight` × the artist's Thompson sample +
/// `novelty_weight` for artists never heard. At most `per_artist_cap` songs
/// per artist and one upload per song survive; MMR orders them, with
/// similarity 1 for one artist and the cosine of the artists' embedding
/// directions otherwise. At most `known_artist_share` × `limit` picks come
/// from heard artists unless new ones run out.
///
/// Every draw and tie-break derives from `week_seed` (see
/// [`week_seed`](crate::week_seed)), so equal inputs give the same list all
/// week and another week reshuffles it. `profile` supplies affinities; from
/// `context` only `exclude` and `now` are read.
pub fn weekly_discovery(
    candidates: Vec<Candidate>,
    profile: &Profile,
    beliefs: &ArtistBeliefs,
    embeddings: &Embeddings,
    context: &RankContext<'_>,
    week_seed: u64,
    config: &WeeklyConfig,
) -> Vec<Candidate> {
    let presets = SourcePresets::new();
    let heard: HashSet<(String, String)> = profile
        .media()
        .iter()
        .map(|item| (artist_key(&item.artist), title_key(&item.title)))
        .collect();
    let directions = artist_vectors(profile, embeddings);
    let mut pool: Vec<Pick<'_>> = candidates
        .into_iter()
        .filter_map(|candidate| {
            let preset = presets.for_media(&candidate.media_id);
            if profile.media_affinity(&candidate.media_id).is_some()
                || !admissible(&candidate, profile, context.exclude, context.now, preset)
            {
                return None;
            }
            let artist = artist_key(&candidate.artist);
            let song = title_key(&candidate.title);
            if !song.is_empty() && heard.contains(&(artist.clone(), song)) {
                return None;
            }
            let known = profile.artist_affinity(&candidate.artist).is_some();
            let novelty = if known { 0.0 } else { config.novelty_weight };
            let relevance = base_score(&candidate, profile, preset, &[])
                + config.exploration_weight * beliefs.sample_keyed(&candidate.artist, week_seed)
                + novelty;
            Some(Pick {
                tie: stable_hash(week_seed, &candidate.media_id),
                traits: SongTraits {
                    direction: directions.get(&artist).map(Vec::as_slice),
                    artist,
                },
                known,
                relevance,
                candidate,
            })
        })
        .collect();
    pool.sort_by(|left, right| {
        right
            .relevance
            .total_cmp(&left.relevance)
            .then(left.tie.cmp(&right.tie))
    });
    let mut distinct = Distinct::default();
    pool.retain(|pick| distinct.admit(&pick.candidate, config.per_artist_cap));
    let indices: Vec<usize> = (0..pool.len()).collect();
    let ordered = mmr(
        &indices,
        |index| pool[*index].relevance,
        |left, right| pool[*left].traits.similarity(&pool[*right].traits),
        config.mmr_lambda,
        pool.len(),
    );
    let quota = known_quota(config);
    let mut picks = Vec::with_capacity(config.limit.min(pool.len()));
    let mut overflow = Vec::new();
    let mut known = 0;
    for index in ordered {
        if pool[index].known {
            if known >= quota {
                overflow.push(index);
                continue;
            }
            known += 1;
        }
        picks.push(index);
    }
    picks.extend(overflow);
    picks.truncate(config.limit);
    take_in_order(pool.into_iter().map(|pick| pick.candidate), picks)
}

fn known_quota(config: &WeeklyConfig) -> usize {
    let share = if config.known_artist_share.is_finite() {
        config.known_artist_share.clamp(0.0, 1.0)
    } else {
        0.0
    };
    (config.limit as f64 * share).round() as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        rng::week_seed,
        test_support::{candidate, completed, skipped, DAY, NOW},
        AffinityConfig, ExploreConfig,
    };

    fn online(id: &str) -> String {
        format!("{}{id}", yt_core::REMOTE_ID_PREFIX)
    }

    struct World {
        profile: Profile,
        beliefs: ArtistBeliefs,
        exclude: HashSet<String>,
    }

    fn world() -> World {
        let at = NOW - 3.0 * DAY;
        let signals = vec![
            completed(&online("heard"), "Known Artist", at),
            completed(&online("heard"), "Known Artist", at + 1.0),
            skipped(&online("bad"), "Skipped Artist", at),
            skipped(&online("bad"), "Skipped Artist", at + 1.0),
        ];
        World {
            profile: Profile::build(&signals, &[], NOW, &AffinityConfig::default()),
            beliefs: ArtistBeliefs::build(&signals, NOW, &ExploreConfig::default()),
            exclude: HashSet::from([online("excluded")]),
        }
    }

    fn candidates() -> Vec<Candidate> {
        let mut reupload = candidate(&online("reupload"), "Known Artist VEVO", 0);
        reupload.title = format!("Song {} (Official Video)", online("heard"));
        let mut mix = candidate(&online("mix"), "Mixer", 0);
        mix.duration = 3.0 * 3_600.0;
        let mut list = vec![
            candidate(&online("heard"), "Known Artist", 0),
            reupload,
            mix,
            candidate(&online("excluded"), "Someone", 0),
            candidate(&online("known-new-1"), "Known Artist", 0),
            candidate(&online("known-new-2"), "Known Artist", 1),
            candidate(&online("skipped-artist"), "Skipped Artist", 0),
        ];
        for artist in 0..14 {
            for song in 0..2 {
                list.push(candidate(
                    &online(&format!("new-{artist}-{song}")),
                    &format!("New Artist {artist}"),
                    song,
                ));
            }
        }
        list
    }

    fn weekly(world: &World, seed: u64, config: &WeeklyConfig) -> Vec<Candidate> {
        let context = RankContext {
            profile: &world.profile,
            exclude: &world.exclude,
            seed_artist: None,
            now: NOW,
            salt: 1,
        };
        weekly_discovery(
            candidates(),
            &world.profile,
            &world.beliefs,
            &Embeddings::default(),
            &context,
            seed,
            config,
        )
    }

    #[test]
    fn weekly_lists_only_unheard_music_once_per_artist() {
        let world = world();
        let picks = weekly(&world, week_seed(NOW, 0, 1), &WeeklyConfig::default());
        let ids: Vec<&str> = picks.iter().map(|pick| pick.media_id.as_str()).collect();
        for forbidden in ["heard", "reupload", "mix", "excluded"] {
            assert!(
                !ids.contains(&online(forbidden).as_str()),
                "{forbidden} listed"
            );
        }
        let artists: HashSet<String> = picks.iter().map(|pick| artist_key(&pick.artist)).collect();
        assert_eq!(artists.len(), picks.len(), "one song per artist");
        assert_eq!(picks.len(), 16);
        let config = WeeklyConfig {
            limit: 5,
            ..WeeklyConfig::default()
        };
        assert_eq!(weekly(&world, 1, &config).len(), 5);
    }

    #[test]
    fn a_week_seed_fixes_the_list_and_another_week_reshuffles_it() {
        let world = world();
        let config = WeeklyConfig::default();
        let this_week = week_seed(NOW, 0, 1);
        let ids = |seed| {
            weekly(&world, seed, &config)
                .into_iter()
                .map(|pick| pick.media_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(this_week), ids(week_seed(NOW + 2.0 * DAY, 0, 1)));
        assert_ne!(ids(this_week), ids(week_seed(NOW + 7.0 * DAY, 0, 1)));
    }

    #[test]
    fn known_artists_are_held_to_their_share_until_new_ones_run_out() {
        let world = world();
        let none = WeeklyConfig {
            known_artist_share: 0.0,
            ..WeeklyConfig::default()
        };
        let picks = weekly(&world, 3, &none);
        let known = |pick: &Candidate| world.profile.artist_affinity(&pick.artist).is_some();
        let first_known = picks.iter().position(known).unwrap();
        assert!(
            picks[first_known..].iter().all(known),
            "known artists only fill the tail"
        );
        let small = WeeklyConfig {
            limit: 10,
            known_artist_share: 0.0,
            ..WeeklyConfig::default()
        };
        assert!(!weekly(&world, 3, &small).iter().any(known));
    }
}
