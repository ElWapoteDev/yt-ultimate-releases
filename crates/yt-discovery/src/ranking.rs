//! Ordering candidate media for Home shelves and autoplay.

use crate::{artist_key, calendar::SECONDS_PER_HOUR, rng::stable_hash, title_key, Profile};
use std::collections::{HashMap, HashSet};
use yt_core::REMOTE_ID_PREFIX;

/// Media skipped this many times is never offered again, by any list.
pub(crate) const SKIP_LIMIT: u32 = 2;
/// Shortest online media accepted as music, in seconds; filters intros and
/// clips.
const MUSIC_MIN_SECONDS: f64 = 60.0;
/// Longest online media accepted as music, in seconds; filters hour-long
/// mixes.
const MUSIC_MAX_SECONDS: f64 = 15.0 * 60.0;

/// One recommendation candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub media_id: String,
    pub title: String,
    pub artist: String,
    /// Seconds; 0 when unknown.
    pub duration: f64,
    /// Position in the provider's list; 0 is the most related.
    pub provider_rank: usize,
}

/// Filters and weights for one kind of ranked list.
#[derive(Debug, Clone, PartialEq)]
pub struct RankingConfig {
    /// Shortest accepted media in seconds; filters intros and clips.
    pub min_duration_seconds: f64,
    /// Longest accepted media in seconds; filters hour-long mixes.
    pub max_duration_seconds: f64,
    /// Whether media of unknown duration (live streams) may appear.
    pub allow_unknown_duration: bool,
    /// Most items of one artist in a ranked list.
    pub per_artist_cap: usize,
    /// Media heard within this many hours is not offered again.
    pub recent_hours: f64,
    /// Media skipped this many times is never offered.
    pub skip_limit: u32,
    /// Provider order weight lost per position.
    pub provider_decay: f64,
    /// Weight of the user's affinity for the candidate's artist.
    pub artist_weight: f64,
    /// Bonus for the seed's own artist, for continuity.
    pub seed_artist_bonus: f64,
    /// Weight of the user's affinity for the candidate itself.
    pub media_weight: f64,
}

impl RankingConfig {
    /// Online lists (Home shelves and online autoplay): music-length media,
    /// provider order matters most, variety across artists.
    pub fn online() -> Self {
        Self {
            min_duration_seconds: MUSIC_MIN_SECONDS,
            max_duration_seconds: MUSIC_MAX_SECONDS,
            allow_unknown_duration: false,
            per_artist_cap: 2,
            recent_hours: 12.0,
            skip_limit: SKIP_LIMIT,
            provider_decay: 0.08,
            artist_weight: 0.35,
            seed_artist_bonus: 0.15,
            media_weight: 0.2,
        }
    }

    /// Autoplay from the local library when online discovery is off: any
    /// length the user chose to keep, continuity with the seed's artist.
    pub fn library_autoplay() -> Self {
        Self {
            min_duration_seconds: 0.0,
            max_duration_seconds: f64::INFINITY,
            allow_unknown_duration: true,
            per_artist_cap: 3,
            recent_hours: 6.0,
            skip_limit: SKIP_LIMIT,
            provider_decay: 0.0,
            artist_weight: 0.4,
            seed_artist_bonus: 0.6,
            media_weight: 0.4,
        }
    }

    /// «Rediscover» shelf: library media liked before but not heard lately,
    /// mixed with media never heard.
    pub fn rediscover() -> Self {
        Self {
            min_duration_seconds: 0.0,
            max_duration_seconds: f64::INFINITY,
            allow_unknown_duration: true,
            per_artist_cap: 2,
            recent_hours: 14.0 * 24.0,
            skip_limit: SKIP_LIMIT,
            provider_decay: 0.0,
            artist_weight: 0.2,
            seed_artist_bonus: 0.0,
            media_weight: 1.0,
        }
    }
}

/// What ranking needs to know about the moment it runs.
#[derive(Debug, Clone, Copy)]
pub struct RankContext<'a> {
    pub profile: &'a Profile,
    /// Media that must not appear: the seed, what plays, what is queued, and
    /// what earlier shelves already show.
    pub exclude: &'a HashSet<String>,
    /// Artist of the seed, for continuity.
    pub seed_artist: Option<&'a str>,
    /// Unix seconds.
    pub now: f64,
    /// Varies the order of equally scored items between sessions while
    /// keeping one session's order stable.
    pub salt: u64,
}

/// The presets that fit a candidate's source: online media are held to
/// music lengths and online weights, library media keep any length.
pub(crate) struct SourcePresets {
    online: RankingConfig,
    library: RankingConfig,
}

impl SourcePresets {
    pub(crate) fn new() -> Self {
        Self {
            online: RankingConfig::online(),
            library: RankingConfig::library_autoplay(),
        }
    }

    /// The preset for a media id: online for remote ids, library otherwise.
    pub(crate) fn for_media(&self, media_id: &str) -> &RankingConfig {
        if media_id.starts_with(REMOTE_ID_PREFIX) {
            &self.online
        } else {
            &self.library
        }
    }
}

/// Provider order as a score: 1 for the provider's first item, falling with
/// `decay` per position.
pub(crate) fn provider_score(provider_rank: usize, decay: f64) -> f64 {
    1.0 / (1.0 + provider_rank as f64 * decay)
}

fn accepted_duration(duration: f64, config: &RankingConfig) -> bool {
    if duration <= 0.0 {
        return config.allow_unknown_duration;
    }
    (config.min_duration_seconds..=config.max_duration_seconds).contains(&duration)
}

/// Whether `candidate` passes `config`'s filters: not in `exclude`, of an
/// accepted length, skipped fewer than `skip_limit` times and not heard
/// within `recent_hours` of `now`.
pub(crate) fn admissible(
    candidate: &Candidate,
    profile: &Profile,
    exclude: &HashSet<String>,
    now: f64,
    config: &RankingConfig,
) -> bool {
    !exclude.contains(&candidate.media_id)
        && accepted_duration(candidate.duration, config)
        && profile
            .media_affinity(&candidate.media_id)
            .is_none_or(|known| {
                known.skips < config.skip_limit
                    && now - known.last_listened >= config.recent_hours * SECONDS_PER_HOUR
            })
}

/// Provider order (1 / (1 + rank × decay)) + artist affinity (tanh) + own
/// affinity (tanh) + a bonus when the artist is one of `seed_artists`
/// (`artist_key`s).
pub(crate) fn base_score(
    candidate: &Candidate,
    profile: &Profile,
    config: &RankingConfig,
    seed_artists: &[String],
) -> f64 {
    let provider = provider_score(candidate.provider_rank, config.provider_decay);
    let artist = config.artist_weight * profile.artist_score(&candidate.artist).tanh();
    let media = profile
        .media_affinity(&candidate.media_id)
        .map_or(0.0, |known| config.media_weight * known.score.tanh());
    let key = artist_key(&candidate.artist);
    let seed = if !key.is_empty() && seed_artists.contains(&key) {
        config.seed_artist_bonus
    } else {
        0.0
    };
    provider + artist + media + seed
}

/// What a list already holds, so it never repeats a media id or a song
/// (`title_key`) and keeps every artist under a cap.
#[derive(Debug, Default)]
pub(crate) struct Distinct {
    ids: HashSet<String>,
    songs: HashSet<String>,
    per_artist: HashMap<String, usize>,
}

impl Distinct {
    /// Record `candidate` and return true when it repeats no id or song and
    /// its artist holds fewer than `cap` items; candidates without an artist
    /// key are never capped. A rejected candidate is not recorded.
    pub(crate) fn admit(&mut self, candidate: &Candidate, cap: usize) -> bool {
        let song = title_key(&candidate.title);
        let artist = artist_key(&candidate.artist);
        let count = self.per_artist.get(&artist).copied().unwrap_or(0);
        if (!artist.is_empty() && count >= cap)
            || self.ids.contains(&candidate.media_id)
            || (!song.is_empty() && self.songs.contains(&song))
        {
            return false;
        }
        self.ids.insert(candidate.media_id.clone());
        if !song.is_empty() {
            self.songs.insert(song);
        }
        self.per_artist.insert(artist, count + 1);
        true
    }
}

/// Rank `candidates` and return at most `limit` of them, best first. The
/// result never repeats a media id or a song (`title_key`), never contains an
/// excluded id, and holds at most `per_artist_cap` items per artist.
pub fn rank(
    candidates: Vec<Candidate>,
    context: &RankContext<'_>,
    config: &RankingConfig,
    limit: usize,
) -> Vec<Candidate> {
    let seed_artists: Vec<String> = context.seed_artist.map(artist_key).into_iter().collect();
    let mut scored: Vec<(f64, u64, Candidate)> = candidates
        .into_iter()
        .filter(|candidate| {
            admissible(
                candidate,
                context.profile,
                context.exclude,
                context.now,
                config,
            )
        })
        .map(|candidate| {
            let score = base_score(&candidate, context.profile, config, &seed_artists);
            let tie = stable_hash(context.salt, &candidate.media_id);
            (score, tie, candidate)
        })
        .collect();
    scored.sort_by(|left, right| right.0.total_cmp(&left.0).then(left.1.cmp(&right.1)));
    let mut distinct = Distinct::default();
    scored
        .into_iter()
        .map(|(_, _, candidate)| candidate)
        .filter(|candidate| distinct.admit(candidate, config.per_artist_cap))
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AffinityConfig, FavoriteSignal};
    use yt_core::{ListenEnd, ListenSignal, ListenSource};

    const NOW: f64 = 1_000_000.0;

    fn candidate(id: &str, title: &str, artist: &str, duration: f64, rank: usize) -> Candidate {
        Candidate {
            media_id: id.into(),
            title: title.into(),
            artist: artist.into(),
            duration,
            provider_rank: rank,
        }
    }

    fn listen(id: &str, artist: &str, hours_ago: f64, heard: f64) -> ListenSignal {
        ListenSignal {
            media_id: id.into(),
            title: id.into(),
            artist: artist.into(),
            remote: true,
            source: ListenSource::User,
            started_at: NOW - hours_ago * SECONDS_PER_HOUR,
            listened_seconds: heard,
            duration: 200.0,
            end: ListenEnd::Interrupted,
        }
    }

    fn context<'a>(profile: &'a Profile, exclude: &'a HashSet<String>) -> RankContext<'a> {
        RankContext {
            profile,
            exclude,
            seed_artist: None,
            now: NOW,
            salt: 7,
        }
    }

    #[test]
    fn filters_exclusions_durations_recent_listens_and_repeated_skips() {
        let signals = [
            listen("recent", "X", 1.0, 200.0),
            listen("skipper", "Y", 100.0, 3.0),
            listen("skipper", "Y", 90.0, 3.0),
            listen("old", "Z", 48.0, 200.0),
        ];
        let profile = Profile::build(&signals, &[], NOW, &AffinityConfig::default());
        let exclude = HashSet::from(["seed".to_owned()]);
        let ranked = rank(
            vec![
                candidate("seed", "Seed", "A", 200.0, 0),
                candidate("short", "Intro", "B", 20.0, 1),
                candidate("long", "Mix 2 hours", "C", 7_200.0, 2),
                candidate("live", "Live now", "D", 0.0, 3),
                candidate("recent", "Recent", "X", 200.0, 4),
                candidate("skipper", "Skipped", "Y", 200.0, 5),
                candidate("old", "Old favorite", "Z", 200.0, 6),
                candidate("fresh", "Fresh", "E", 200.0, 7),
            ],
            &context(&profile, &exclude),
            &RankingConfig::online(),
            10,
        );
        let ids: Vec<_> = ranked.iter().map(|item| item.media_id.as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"old") && ids.contains(&"fresh"));
    }

    #[test]
    fn duplicates_collapse_and_artists_are_capped() {
        let profile = Profile::default();
        let exclude = HashSet::new();
        let ranked = rank(
            vec![
                candidate("a", "Bailando", "Artist", 200.0, 0),
                candidate(
                    "b",
                    "Artist - Bailando (Official Video)",
                    "ArtistVEVO",
                    200.0,
                    1,
                ),
                candidate("c", "Second", "Artist", 200.0, 2),
                candidate("d", "Third", "Artist - Topic", 200.0, 3),
                candidate("e", "Other", "Someone", 200.0, 4),
                candidate("a", "Bailando", "Artist", 200.0, 5),
            ],
            &context(&profile, &exclude),
            &RankingConfig::online(),
            10,
        );
        let ids: Vec<_> = ranked.iter().map(|item| item.media_id.as_str()).collect();
        assert_eq!(ids, ["a", "c", "e"]);
        assert_eq!(
            rank(
                ranked.clone(),
                &context(&profile, &exclude),
                &RankingConfig::online(),
                1
            )
            .len(),
            1
        );
    }

    #[test]
    fn liked_artists_and_seed_continuity_lift_candidates() {
        let favorites = [FavoriteSignal {
            media_id: "fav".into(),
            title: "Fav".into(),
            artist: "Loved".into(),
            remote: true,
            added_at: NOW,
        }];
        let profile = Profile::build(&[], &favorites, NOW, &AffinityConfig::default());
        let exclude = HashSet::new();
        let candidates = vec![
            candidate("first", "First", "Stranger", 200.0, 0),
            candidate("liked", "Liked", "Loved", 200.0, 2),
        ];
        let ranked = rank(
            candidates.clone(),
            &context(&profile, &exclude),
            &RankingConfig::online(),
            2,
        );
        assert_eq!(ranked[0].media_id, "liked");
        let neutral = Profile::default();
        let mut continuity = context(&neutral, &exclude);
        continuity.seed_artist = Some("Stranger");
        let ranked = rank(
            candidates,
            &continuity,
            &RankingConfig::library_autoplay(),
            2,
        );
        assert_eq!(ranked[0].media_id, "first");
    }

    #[test]
    fn ties_are_stable_for_a_salt_and_vary_between_salts() {
        let profile = Profile::default();
        let exclude = HashSet::new();
        let candidates: Vec<_> = (0..12)
            .map(|index| {
                candidate(
                    &format!("id{index}"),
                    &format!("Song {index}"),
                    &format!("A{index}"),
                    0.0,
                    0,
                )
            })
            .collect();
        let order = |salt| {
            let mut context = context(&profile, &exclude);
            context.salt = salt;
            rank(
                candidates.clone(),
                &context,
                &RankingConfig::rediscover(),
                12,
            )
            .into_iter()
            .map(|item| item.media_id)
            .collect::<Vec<_>>()
        };
        assert_eq!(order(1), order(1));
        assert_ne!(order(1), order(2));
        assert_eq!(stable_hash(0, "x"), stable_hash(0, "x"));
    }

    #[test]
    fn presets_follow_the_source_of_the_media() {
        let presets = SourcePresets::new();
        let online = format!("{REMOTE_ID_PREFIX}aqz-KE-bpKQ");
        assert_eq!(presets.for_media(&online), &RankingConfig::online());
        assert_eq!(
            presets.for_media("local-track"),
            &RankingConfig::library_autoplay()
        );
    }
}
