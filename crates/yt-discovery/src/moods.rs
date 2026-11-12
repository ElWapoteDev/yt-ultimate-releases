//! Moods measured from audio: a song's place on Russell's (1980) circumplex
//! of affect, valence × arousal, and its soft membership in six listening
//! moods.
//!
//! The cues follow the music-emotion literature (Gabrielsson & Lindström
//! 2010; Yang & Chen 2012): loudness, onset density and tempo carry arousal;
//! mode is the strongest structural cue for valence, with fast tempo and
//! bright timbre leaning pleasant.

use crate::{
    artist_key,
    diversity::{mmr, SongTraits},
    ranking::SKIP_LIMIT,
    Profile,
};
use std::collections::HashMap;
use yt_core::AudioFeatures;

/// Mean level of quiet music, in dBFS.
const QUIET_DBFS: f64 = -30.0;
/// Mean level of a loud modern master, in dBFS.
const LOUD_DBFS: f64 = -9.0;
/// Tempo felt as slow, in BPM.
const SLOW_BPM: f64 = 60.0;
/// Tempo felt as fast, in BPM.
const FAST_BPM: f64 = 180.0;
/// Onsets per second of sparse music.
const SPARSE_ONSETS_PER_SECOND: f64 = 1.0;
/// Onsets per second of busy music.
const BUSY_ONSETS_PER_SECOND: f64 = 6.0;
/// Spectral centroid of a dark timbre, in Hz.
const DARK_CENTROID_HZ: f64 = 700.0;
/// Spectral centroid of a bright timbre, in Hz.
const BRIGHT_CENTROID_HZ: f64 = 3_300.0;
/// Tonality (major minus minor key correlation) of a clear key; a key and
/// its relative minor rarely differ by more.
const CLEAR_KEY_MODE: f64 = 0.25;
/// Arousal weight of loudness; the four arousal weights sum to 1.
const AROUSAL_LOUDNESS: f64 = 0.35;
/// Arousal weight of onset density.
const AROUSAL_ONSETS: f64 = 0.3;
/// Arousal weight of tempo.
const AROUSAL_TEMPO: f64 = 0.25;
/// Arousal weight of brightness.
const AROUSAL_BRIGHTNESS: f64 = 0.1;
/// Valence weight of mode; the three valence weights sum to 1.
const VALENCE_MODE: f64 = 0.6;
/// Valence weight of tempo.
const VALENCE_TEMPO: f64 = 0.25;
/// Valence weight of brightness.
const VALENCE_BRIGHTNESS: f64 = 0.15;
/// Spread σ of a mood around its prototype, in circumplex units:
/// memberships are a softmax of −d² / (2σ²).
const MEMBERSHIP_BANDWIDTH: f64 = 0.35;
/// MMR λ of a mood mix: high relevance, with a song by an artist already in
/// the mix losing ground to other artists.
const MIX_LAMBDA: f64 = 0.7;

/// A listening mood.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mood {
    /// Driving and alert: high arousal, mildly positive.
    Energetic,
    /// Euphoric and danceable: high arousal, high valence.
    Party,
    /// Relaxed and serene: low arousal, positive valence.
    Calm,
    /// Unobtrusive: near-neutral valence, somewhat low arousal.
    Focus,
    /// Sad and reflective: low arousal, negative valence.
    Melancholic,
    /// Tense and aggressive: high arousal, negative valence.
    Intense,
}

impl Mood {
    /// Every mood, in display order.
    pub const ALL: [Mood; 6] = [
        Mood::Energetic,
        Mood::Party,
        Mood::Calm,
        Mood::Focus,
        Mood::Melancholic,
        Mood::Intense,
    ];

    /// Stable identifier for keys and storage.
    pub fn key(self) -> &'static str {
        match self {
            Mood::Energetic => "energetic",
            Mood::Party => "party",
            Mood::Calm => "calm",
            Mood::Focus => "focus",
            Mood::Melancholic => "melancholic",
            Mood::Intense => "intense",
        }
    }

    /// The mood whose [`key`](Self::key) is `key`.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mood| mood.key() == key)
    }

    /// The mood's prototype on the circumplex, placed at the angle of the
    /// affect words Russell (1980) found around it: «excited/delighted» for
    /// Party, «aroused/alert» for Energetic, «tense/angry» for Intense,
    /// «sad/gloomy» for Melancholic and «relaxed/serene» for Calm; Focus sits
    /// near the neutral center, slightly below it.
    pub fn prototype(self) -> MoodPoint {
        let (valence, arousal) = match self {
            Mood::Energetic => (0.25, 0.9),
            Mood::Party => (0.75, 0.65),
            Mood::Calm => (0.5, -0.65),
            Mood::Focus => (0.05, -0.2),
            Mood::Melancholic => (-0.65, -0.55),
            Mood::Intense => (-0.6, 0.75),
        };
        MoodPoint { valence, arousal }
    }
}

/// A point of the valence × arousal plane, both in [-1, 1].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoodPoint {
    /// Unpleasant (−1) to pleasant (1).
    pub valence: f64,
    /// Calm (−1) to activated (1).
    pub arousal: f64,
}

impl MoodPoint {
    fn distance_squared(self, other: MoodPoint) -> f64 {
        (self.valence - other.valence).powi(2) + (self.arousal - other.arousal).powi(2)
    }
}

/// Where measured audio sits on the circumplex.
///
/// Each feature is placed on [-1, 1] within its typical span, clamped
/// outside it: loudness −30 to −9 dBFS, tempo 60 to 180 BPM, 1 to 6 onsets
/// per second, spectral centroid 700 Hz to 3.3 kHz, tonality ±0.25. Arousal =
/// 0.35 loudness + 0.3 onset density + 0.25 tempo + 0.1 brightness; valence =
/// 0.6 mode + 0.25 tempo + 0.15 brightness. Unknown tempo (0) is neutral.
/// Features are expected to pass `AudioFeatures::validate`.
pub fn mood_point(features: &AudioFeatures) -> MoodPoint {
    let tempo = if features.tempo_bpm > 0.0 {
        position(features.tempo_bpm, SLOW_BPM, FAST_BPM)
    } else {
        0.0
    };
    let brightness = position(
        features.brightness,
        brightness_at(DARK_CENTROID_HZ),
        brightness_at(BRIGHT_CENTROID_HZ),
    );
    let loudness = position(features.energy, energy_at(QUIET_DBFS), energy_at(LOUD_DBFS));
    let onsets = position(
        features.onset_rate,
        SPARSE_ONSETS_PER_SECOND / AudioFeatures::FULL_ONSET_RATE,
        BUSY_ONSETS_PER_SECOND / AudioFeatures::FULL_ONSET_RATE,
    );
    let mode = position(features.mode, -CLEAR_KEY_MODE, CLEAR_KEY_MODE);
    MoodPoint {
        valence: VALENCE_MODE * mode + VALENCE_TEMPO * tempo + VALENCE_BRIGHTNESS * brightness,
        arousal: AROUSAL_LOUDNESS * loudness
            + AROUSAL_ONSETS * onsets
            + AROUSAL_TEMPO * tempo
            + AROUSAL_BRIGHTNESS * brightness,
    }
}

/// Where `value` sits between `low` and `high`, mapped linearly onto
/// [-1, 1] and clamped.
fn position(value: f64, low: f64, high: f64) -> f64 {
    (2.0 * (value - low) / (high - low) - 1.0).clamp(-1.0, 1.0)
}

/// `AudioFeatures::energy` of a mean level in dBFS.
fn energy_at(dbfs: f64) -> f64 {
    let range = AudioFeatures::ENERGY_RANGE_DBFS;
    (dbfs - range.start()) / (range.end() - range.start())
}

/// `AudioFeatures::brightness` of a spectral centroid in Hz.
fn brightness_at(centroid_hz: f64) -> f64 {
    let range = AudioFeatures::BRIGHTNESS_RANGE_HZ;
    (centroid_hz / range.start()).ln() / (range.end() / range.start()).ln()
}

/// Soft membership of measured audio in every mood, in [`Mood::ALL`] order,
/// summing to 1: a softmax of −d² / (2 · 0.35²) over the squared distances d²
/// from [`mood_point`] to each [`Mood::prototype`].
pub fn mood_membership(features: &AudioFeatures) -> [(Mood, f64); 6] {
    let point = mood_point(features);
    let logits = Mood::ALL.map(|mood| {
        -point.distance_squared(mood.prototype()) / (2.0 * MEMBERSHIP_BANDWIDTH.powi(2))
    });
    let peak = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let weights = logits.map(|logit| (logit - peak).exp());
    let total: f64 = weights.iter().sum();
    let mut memberships = Mood::ALL.map(|mood| (mood, 0.0));
    for ((_, membership), weight) in memberships.iter_mut().zip(weights) {
        *membership = weight / total;
    }
    memberships
}

struct MixPick<'a> {
    media_id: &'a str,
    traits: SongTraits<'a>,
    relevance: f64,
}

/// Up to `limit` media ids of `features` for `mood`, best first.
///
/// Only media whose membership in `mood` is at least `min_membership` take
/// part, and media skipped twice or more never do. Liked media (positive
/// affinity) come first, ranked by membership × tanh(affinity); the rest
/// follow, ranked by membership × (1 + tanh(affinity)), media the profile
/// does not know counting as neutral. Within each group, maximal marginal
/// relevance (λ 0.7) spreads artists. Ties break by id.
pub fn mood_mix(
    mood: Mood,
    profile: &Profile,
    features: &HashMap<String, AudioFeatures>,
    limit: usize,
    min_membership: f64,
) -> Vec<String> {
    let mut liked = Vec::new();
    let mut others = Vec::new();
    for (media_id, measured) in features {
        let fit = mood_membership(measured)
            .iter()
            .find(|(candidate, _)| *candidate == mood)
            .map_or(0.0, |(_, membership)| *membership);
        if fit.is_nan() || fit < min_membership {
            continue;
        }
        let known = profile.media_affinity(media_id);
        if known.is_some_and(|known| known.skips >= SKIP_LIMIT) {
            continue;
        }
        let score = known.map_or(0.0, |known| known.score);
        let pick = MixPick {
            media_id,
            traits: SongTraits {
                artist: known
                    .map(|known| artist_key(&known.artist))
                    .unwrap_or_default(),
                direction: None,
            },
            relevance: if score > 0.0 {
                fit * score.tanh()
            } else {
                fit * (1.0 + score.tanh())
            },
        };
        if score > 0.0 {
            liked.push(pick);
        } else {
            others.push(pick);
        }
    }
    let mut mix = spread(liked, limit);
    let remaining = limit.saturating_sub(mix.len());
    mix.extend(spread(others, remaining));
    mix
}

/// `picks` by relevance with artists spread by MMR, at most `limit`.
fn spread(mut picks: Vec<MixPick<'_>>, limit: usize) -> Vec<String> {
    picks.sort_by(|left, right| {
        right
            .relevance
            .total_cmp(&left.relevance)
            .then_with(|| left.media_id.cmp(right.media_id))
    });
    let order: Vec<usize> = (0..picks.len()).collect();
    mmr(
        &order,
        |index| picks[*index].relevance,
        |left, right| picks[*left].traits.similarity(&picks[*right].traits),
        MIX_LAMBDA,
        limit,
    )
    .into_iter()
    .map(|index| picks[index].media_id.to_owned())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        rng::SeededRng,
        test_support::{completed, skipped, DAY, NOW},
        AffinityConfig,
    };

    fn features(
        energy: f64,
        tempo_bpm: f64,
        onsets: f64,
        brightness: f64,
        mode: f64,
    ) -> AudioFeatures {
        AudioFeatures {
            analyzed_seconds: 60.0,
            energy,
            dynamics: 0.2,
            brightness,
            onset_rate: onsets,
            tempo_bpm,
            mode,
        }
    }

    fn loud_fast_major_bright() -> AudioFeatures {
        features(0.85, 150.0, 0.75, 0.75, 0.25)
    }

    fn quiet_slow_minor() -> AudioFeatures {
        features(0.5, 65.0, 0.15, 0.45, -0.25)
    }

    fn quiet_slow_major() -> AudioFeatures {
        features(0.5, 65.0, 0.15, 0.6, 0.25)
    }

    fn strongest(features: &AudioFeatures) -> Mood {
        mood_membership(features)
            .into_iter()
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(mood, _)| mood)
            .unwrap()
    }

    #[test]
    fn the_circumplex_orders_songs_as_the_literature_expects() {
        let party = mood_point(&loud_fast_major_bright());
        let sad = mood_point(&quiet_slow_minor());
        let calm = mood_point(&quiet_slow_major());
        assert!(party.valence > 0.5 && party.arousal > 0.5);
        assert!(sad.valence < -0.5 && sad.arousal < -0.5);
        assert!(calm.valence > 0.0 && calm.arousal < -0.5);
        assert!(calm.valence > sad.valence && party.arousal > calm.arousal);
        assert!(matches!(
            strongest(&loud_fast_major_bright()),
            Mood::Party | Mood::Energetic
        ));
        assert_eq!(strongest(&quiet_slow_minor()), Mood::Melancholic);
        assert_eq!(strongest(&quiet_slow_major()), Mood::Calm);
        assert_eq!(
            strongest(&features(0.85, 165.0, 0.8, 0.8, -0.25)),
            Mood::Intense
        );
        let unknown_tempo = mood_point(&features(0.7, 0.0, 0.4, 0.6, 0.0));
        assert!(unknown_tempo.valence.abs() < 0.1);
    }

    #[test]
    fn memberships_sum_to_one_and_keys_round_trip() {
        let mut rng = SeededRng::new(9);
        for _ in 0..200 {
            let measured = features(
                rng.next_f64(),
                if rng.next_f64() < 0.1 {
                    0.0
                } else {
                    40.0 + 180.0 * rng.next_f64()
                },
                rng.next_f64(),
                rng.next_f64(),
                2.0 * rng.next_f64() - 1.0,
            );
            let memberships = mood_membership(&measured);
            let total: f64 = memberships.iter().map(|(_, value)| value).sum();
            assert!((total - 1.0).abs() < 1e-12);
            assert!(memberships
                .iter()
                .all(|(_, value)| (0.0..=1.0).contains(value)));
        }
        for mood in Mood::ALL {
            assert_eq!(Mood::from_key(mood.key()), Some(mood));
        }
        assert_eq!(Mood::from_key("unknown"), None);
    }

    #[test]
    fn mood_mixes_put_liked_fitting_songs_first_and_never_repeated_skips() {
        let at = NOW - DAY;
        let signals = vec![
            completed("calm-loved", "Quiet A", at),
            completed("calm-loved", "Quiet A", at),
            completed("calm-liked", "Quiet B", at),
            completed("calm-same-artist", "Quiet A", at),
            skipped("calm-skipped", "Quiet C", at),
            skipped("calm-skipped", "Quiet C", at + 1.0),
            completed("party-liked", "Loud", at),
        ];
        let profile = Profile::build(&signals, &[], NOW, &AffinityConfig::default());
        let features = HashMap::from([
            ("calm-loved".to_owned(), quiet_slow_major()),
            ("calm-liked".to_owned(), quiet_slow_major()),
            ("calm-same-artist".to_owned(), quiet_slow_major()),
            ("calm-skipped".to_owned(), quiet_slow_major()),
            ("calm-unheard".to_owned(), quiet_slow_major()),
            ("party-liked".to_owned(), loud_fast_major_bright()),
        ]);
        let mix = mood_mix(Mood::Calm, &profile, &features, 10, 0.3);
        assert_eq!(
            mix,
            [
                "calm-loved",
                "calm-liked",
                "calm-same-artist",
                "calm-unheard"
            ]
        );
        assert_eq!(mood_mix(Mood::Calm, &profile, &features, 2, 0.3).len(), 2);
        assert_eq!(
            mood_mix(Mood::Party, &profile, &features, 10, 0.3),
            ["party-liked"]
        );
    }
}
