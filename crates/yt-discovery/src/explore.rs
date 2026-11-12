//! Beliefs about how much the user enjoys each artist, as Beta posteriors
//! that Thompson sampling draws from to balance familiar and new artists
//! (Chapelle & Li 2011; McInerney et al. 2018).

use crate::{
    artist_key,
    calendar::half_life_weight,
    rng::{mix_seed, stable_hash, SeededRng},
    ListenOutcome, ListenPolicy,
};
use std::collections::HashMap;
use yt_core::ListenSignal;

/// Shape of the uniform Beta(1, 1) prior: every enjoyment rate equally
/// likely before any listen.
const UNIFORM_PRIOR: f64 = 1.0;

/// How listens become evidence about artists.
#[derive(Debug, Clone, PartialEq)]
pub struct ExploreConfig {
    /// Thresholds that judge each listen a completion, partial or skip.
    pub listen: ListenPolicy,
    /// Successes every artist starts with (Beta α₀); must be finite and at
    /// least [`SeededRng::MIN_SHAPE`], otherwise 1 is used.
    pub prior_successes: f64,
    /// Failures every artist starts with (Beta β₀); same bounds.
    pub prior_failures: f64,
    /// Success credited to a play of unknown length (imported history).
    pub played_success: f64,
    /// Days after which a listen counts half, so beliefs follow changing
    /// taste (a discounted bandit).
    pub half_life_days: f64,
}

impl Default for ExploreConfig {
    fn default() -> Self {
        Self {
            listen: ListenPolicy::default(),
            prior_successes: UNIFORM_PRIOR,
            prior_failures: UNIFORM_PRIOR,
            played_success: 0.5,
            half_life_days: 90.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Evidence {
    successes: f64,
    failures: f64,
}

/// A Beta posterior of how often each artist's songs are enjoyed, by
/// `artist_key`. Artists never heard keep the prior.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistBeliefs {
    artists: HashMap<String, Evidence>,
    prior: Evidence,
}

impl Default for ArtistBeliefs {
    /// Beliefs without evidence: every artist has the uniform prior.
    fn default() -> Self {
        Self {
            artists: HashMap::new(),
            prior: Evidence {
                successes: UNIFORM_PRIOR,
                failures: UNIFORM_PRIOR,
            },
        }
    }
}

impl ArtistBeliefs {
    /// Collect evidence from `signals` (any order) at `now` (Unix seconds).
    ///
    /// A complete listen is one success, a partial or ongoing one adds its
    /// share heard to the successes, a play of unknown length adds
    /// `played_success`, and a skip is one failure. Every listen decays with
    /// `half_life_days`. Listens without an artist key or with non-finite
    /// facts are ignored.
    pub fn build(signals: &[ListenSignal], now: f64, config: &ExploreConfig) -> Self {
        let mut artists: HashMap<String, Evidence> = HashMap::new();
        for signal in signals {
            let (successes, failures) = match config.listen.classify(signal) {
                ListenOutcome::Completed => (1.0, 0.0),
                ListenOutcome::Partial(share) | ListenOutcome::Ongoing(share) => (share, 0.0),
                ListenOutcome::Played => (config.played_success, 0.0),
                ListenOutcome::Skipped => (0.0, 1.0),
            };
            let key = artist_key(&signal.artist);
            if key.is_empty() || !(successes + failures).is_finite() {
                continue;
            }
            let weight = half_life_weight(now - signal.started_at, config.half_life_days);
            let evidence = artists.entry(key).or_default();
            evidence.successes += successes * weight;
            evidence.failures += failures * weight;
        }
        Self {
            artists,
            prior: Evidence {
                successes: prior_shape(config.prior_successes),
                failures: prior_shape(config.prior_failures),
            },
        }
    }

    /// Beta parameters (α, β) of `artist` (a display name): the prior plus
    /// the artist's decayed successes and failures.
    pub fn posterior(&self, artist: &str) -> (f64, f64) {
        let evidence = self.evidence_of(artist);
        (
            self.prior.successes + evidence.successes,
            self.prior.failures + evidence.failures,
        )
    }

    /// Expected enjoyment rate of `artist`, α / (α + β), in (0, 1).
    pub fn mean(&self, artist: &str) -> f64 {
        let (alpha, beta) = self.posterior(artist);
        alpha / (alpha + beta)
    }

    /// Decayed listens observed for `artist` (successes plus failures); 0
    /// for artists never heard.
    pub fn evidence(&self, artist: &str) -> f64 {
        let evidence = self.evidence_of(artist);
        evidence.successes + evidence.failures
    }

    /// A Thompson sample of `artist`'s enjoyment rate from its posterior, in
    /// [0, 1]; unknown artists draw from the prior.
    pub fn sample(&self, artist: &str, rng: &mut SeededRng) -> f64 {
        let (alpha, beta) = self.posterior(artist);
        rng.beta(alpha, beta)
    }

    /// A Thompson sample drawn from a generator seeded by `seed` and the
    /// artist's key, so one artist's draw never depends on which other
    /// artists are sampled or in what order.
    pub(crate) fn sample_keyed(&self, artist: &str, seed: u64) -> f64 {
        let mut rng = SeededRng::new(mix_seed(seed, stable_hash(seed, &artist_key(artist))));
        self.sample(artist, &mut rng)
    }

    fn evidence_of(&self, artist: &str) -> Evidence {
        self.artists
            .get(&artist_key(artist))
            .copied()
            .unwrap_or_default()
    }
}

fn prior_shape(value: f64) -> f64 {
    if value.is_finite() && value >= SeededRng::MIN_SHAPE {
        value
    } else {
        UNIFORM_PRIOR
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{completed, skipped, DAY, NOW};

    const DRAWS: usize = 4_000;

    fn beliefs() -> ArtistBeliefs {
        let mut signals = Vec::new();
        for day in 0..6 {
            let at = NOW - f64::from(day) * DAY;
            signals.push(completed(&format!("loved{day}"), "Loved", at));
            signals.push(skipped(&format!("skipped{day}"), "Skipped", at));
        }
        ArtistBeliefs::build(&signals, NOW, &ExploreConfig::default())
    }

    #[test]
    fn completions_raise_and_skips_lower_an_artist_in_expectation() {
        let beliefs = beliefs();
        let mut rng = SeededRng::new(5);
        let mut wins = 0;
        let (mut loved, mut skipped, mut unknown) = (0.0, 0.0, 0.0);
        for _ in 0..DRAWS {
            let a = beliefs.sample("Loved", &mut rng);
            let b = beliefs.sample("Skipped - Topic", &mut rng);
            wins += usize::from(a > b);
            loved += a;
            skipped += b;
            unknown += beliefs.sample("Never Heard", &mut rng);
        }
        let draws = DRAWS as f64;
        assert!(wins as f64 / draws > 0.95);
        assert!(loved / draws > 0.8 && skipped / draws < 0.2);
        assert!(
            (unknown / draws - 0.5).abs() < 0.03,
            "unknown artists keep the prior"
        );
        assert!((beliefs.mean("Loved") - loved / draws).abs() < 0.02);
        assert_eq!(beliefs.posterior("Never Heard"), (1.0, 1.0));
        assert_eq!(beliefs.evidence("Never Heard"), 0.0);
        assert!(beliefs.evidence("Loved") > 5.0);
    }

    #[test]
    fn keyed_samples_are_stable_per_artist_and_seed() {
        let beliefs = beliefs();
        let first = beliefs.sample_keyed("Loved", 1);
        assert_eq!(beliefs.sample_keyed("LOVED", 1), first);
        assert_ne!(beliefs.sample_keyed("Loved", 2), first);
        let invalid = ExploreConfig {
            prior_successes: f64::NAN,
            prior_failures: -3.0,
            ..ExploreConfig::default()
        };
        let fallback = ArtistBeliefs::build(&[], NOW, &invalid);
        assert_eq!(fallback.posterior("Anyone"), (1.0, 1.0));
        assert_eq!(ArtistBeliefs::default(), fallback);
    }
}
