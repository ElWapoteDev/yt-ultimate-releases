//! What the user likes, from listening facts and favorites.

use crate::{artist_key, calendar::half_life_weight, ListenOutcome, ListenPolicy};
use std::collections::{HashMap, HashSet};
use yt_core::{ListenSignal, ListenSource};

/// Weights and decay that turn listens into affinity.
#[derive(Debug, Clone, PartialEq)]
pub struct AffinityConfig {
    pub listen: ListenPolicy,
    /// Score of a complete listen.
    pub completed: f64,
    /// Score per share heard of a partial listen.
    pub partial: f64,
    /// Score of a skip (negative).
    pub skipped: f64,
    /// Score of a play with unknown length (imported history).
    pub played: f64,
    /// Multiplier for positive scores of media autoplay picked, since the
    /// user did not choose it.
    pub autoplay_positive: f64,
    /// Days after which a listen counts half.
    pub half_life_days: f64,
    /// Score of being a favorite.
    pub favorite: f64,
    /// Days after which a favorite counts half; favorites are durable taste.
    pub favorite_half_life_days: f64,
    /// Days after which a listen or a favorite counts half in the short-term
    /// score, which follows what the user plays right now.
    pub short_term_half_life_days: f64,
}

impl Default for AffinityConfig {
    fn default() -> Self {
        Self {
            listen: ListenPolicy::default(),
            completed: 1.0,
            partial: 0.6,
            skipped: -0.6,
            played: 0.5,
            autoplay_positive: 0.7,
            half_life_days: 30.0,
            favorite: 3.0,
            favorite_half_life_days: 365.0,
            short_term_half_life_days: 3.0,
        }
    }
}

impl AffinityConfig {
    /// Score one listen adds before decay: `completed` for a complete
    /// listen, `partial` × share for a partial or ongoing one, `skipped` for
    /// a skip and `played` for a play of unknown length; positive scores of
    /// autoplay picks are scaled by `autoplay_positive`.
    pub fn listen_score(&self, signal: &ListenSignal) -> f64 {
        self.outcome_score(self.listen.classify(signal), signal.source)
    }

    fn outcome_score(&self, outcome: ListenOutcome, source: ListenSource) -> f64 {
        let base = match outcome {
            ListenOutcome::Completed => self.completed,
            ListenOutcome::Partial(ratio) | ListenOutcome::Ongoing(ratio) => self.partial * ratio,
            ListenOutcome::Skipped => self.skipped,
            ListenOutcome::Played => self.played,
        };
        if base > 0.0 && source == ListenSource::Autoplay {
            base * self.autoplay_positive
        } else {
            base
        }
    }
}

/// A favorite as ranking sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct FavoriteSignal {
    pub media_id: String,
    pub title: String,
    pub artist: String,
    pub remote: bool,
    /// Unix seconds.
    pub added_at: f64,
}

/// Affinity for one media item.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaAffinity {
    pub media_id: String,
    pub title: String,
    pub artist: String,
    pub remote: bool,
    /// Long-term affinity: listens decay with `half_life_days`, favorites
    /// with `favorite_half_life_days`.
    pub score: f64,
    /// Short-term affinity: the same listens and favorites decayed with
    /// `short_term_half_life_days`, so recent bursts dominate.
    pub short_term: f64,
    pub listens: u32,
    pub skips: u32,
    /// Unix seconds of the latest listen; 0 when never listened.
    pub last_listened: f64,
    pub favorite: bool,
}

/// Affinity for one artist, grouped by `artist_key`.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistAffinity {
    pub key: String,
    /// Most recent display name seen for the artist.
    pub name: String,
    /// Sum of the long-term scores of the artist's media.
    pub score: f64,
    /// Sum of the short-term scores of the artist's media.
    pub short_term: f64,
}

/// The user's listening profile at one instant.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profile {
    media: Vec<MediaAffinity>,
    artists: Vec<ArtistAffinity>,
    media_index: HashMap<String, usize>,
    artist_index: HashMap<String, usize>,
    config: AffinityConfig,
}

fn entry<'a>(
    media: &'a mut HashMap<String, MediaAffinity>,
    id: &str,
    title: &str,
    artist: &str,
    remote: bool,
) -> &'a mut MediaAffinity {
    media.entry(id.to_owned()).or_insert_with(|| MediaAffinity {
        media_id: id.to_owned(),
        title: title.to_owned(),
        artist: artist.to_owned(),
        remote,
        score: 0.0,
        short_term: 0.0,
        listens: 0,
        skips: 0,
        last_listened: 0.0,
        favorite: false,
    })
}

impl Profile {
    /// Build the profile. `signals` may be in any order; `now` is Unix seconds.
    pub fn build(
        signals: &[ListenSignal],
        favorites: &[FavoriteSignal],
        now: f64,
        config: &AffinityConfig,
    ) -> Self {
        let mut media: HashMap<String, MediaAffinity> = HashMap::new();
        for signal in signals {
            let outcome = config.listen.classify(signal);
            let base = config.outcome_score(outcome, signal.source);
            let age = now - signal.started_at;
            let item = entry(
                &mut media,
                &signal.media_id,
                &signal.title,
                &signal.artist,
                signal.remote,
            );
            item.score += base * half_life_weight(age, config.half_life_days);
            item.short_term += base * half_life_weight(age, config.short_term_half_life_days);
            item.listens += 1;
            if outcome == ListenOutcome::Skipped {
                item.skips += 1;
            }
            item.last_listened = item.last_listened.max(signal.started_at);
        }
        for favorite in favorites {
            let age = now - favorite.added_at;
            let item = entry(
                &mut media,
                &favorite.media_id,
                &favorite.title,
                &favorite.artist,
                favorite.remote,
            );
            item.favorite = true;
            item.score += config.favorite * half_life_weight(age, config.favorite_half_life_days);
            item.short_term +=
                config.favorite * half_life_weight(age, config.short_term_half_life_days);
        }
        let mut media: Vec<_> = media.into_values().collect();
        media.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.media_id.cmp(&right.media_id))
        });
        let mut artists: HashMap<String, ArtistAffinity> = HashMap::new();
        for item in &media {
            let key = artist_key(&item.artist);
            if key.is_empty() {
                continue;
            }
            let artist = artists
                .entry(key.clone())
                .or_insert_with(|| ArtistAffinity {
                    key,
                    name: item.artist.clone(),
                    score: 0.0,
                    short_term: 0.0,
                });
            artist.score += item.score;
            artist.short_term += item.short_term;
        }
        let mut artists: Vec<_> = artists.into_values().collect();
        artists.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.key.cmp(&right.key))
        });
        let media_index = media
            .iter()
            .enumerate()
            .map(|(index, item)| (item.media_id.clone(), index))
            .collect();
        let artist_index = artists
            .iter()
            .enumerate()
            .map(|(index, artist)| (artist.key.clone(), index))
            .collect();
        Self {
            media,
            artists,
            media_index,
            artist_index,
            config: config.clone(),
        }
    }

    /// Media by descending affinity.
    pub fn media(&self) -> &[MediaAffinity] {
        &self.media
    }

    /// Artists by descending affinity.
    pub fn artists(&self) -> &[ArtistAffinity] {
        &self.artists
    }

    pub fn media_affinity(&self, media_id: &str) -> Option<&MediaAffinity> {
        self.media_index
            .get(media_id)
            .map(|index| &self.media[*index])
    }

    /// Affinity of an artist by display name, grouped by `artist_key`;
    /// `None` for artists the user never heard or favorited.
    pub fn artist_affinity(&self, artist: &str) -> Option<&ArtistAffinity> {
        self.artist_index
            .get(&artist_key(artist))
            .map(|index| &self.artists[*index])
    }

    /// Affinity of an artist by display name; 0 for unknown artists.
    pub fn artist_score(&self, artist: &str) -> f64 {
        self.artist_affinity(artist)
            .map_or(0.0, |known| known.score)
    }

    /// The configuration the profile was built with, so other engines judge
    /// listens the same way.
    pub fn config(&self) -> &AffinityConfig {
        &self.config
    }

    /// Up to `count` liked media to seed recommendations, at most one per
    /// artist so one artist cannot fill every shelf.
    pub fn seeds(&self, count: usize) -> Vec<&MediaAffinity> {
        let mut used_artists = HashSet::new();
        self.media
            .iter()
            .filter(|item| item.score > 0.0)
            .filter(|item| {
                let key = artist_key(&item.artist);
                key.is_empty() || used_artists.insert(key)
            })
            .take(count)
            .collect()
    }

    /// Up to `count` liked artists.
    pub fn top_artists(&self, count: usize) -> Vec<&ArtistAffinity> {
        self.artists
            .iter()
            .filter(|artist| artist.score > 0.0)
            .take(count)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::SECONDS_PER_DAY;
    use yt_core::ListenEnd;

    const NOW: f64 = 100.0 * SECONDS_PER_DAY;

    fn signal(id: &str, artist: &str, days_ago: f64, heard: f64, end: ListenEnd) -> ListenSignal {
        ListenSignal {
            media_id: id.into(),
            title: format!("Song {id}"),
            artist: artist.into(),
            remote: false,
            source: ListenSource::User,
            started_at: NOW - days_ago * SECONDS_PER_DAY,
            listened_seconds: heard,
            duration: 200.0,
            end,
        }
    }

    #[test]
    fn completions_favorites_and_recency_raise_affinity_while_skips_lower_it() {
        let signals = [
            signal("loved", "Artist A", 1.0, 200.0, ListenEnd::Finished),
            signal("loved", "Artist A", 2.0, 200.0, ListenEnd::Finished),
            signal("old", "Artist B", 90.0, 200.0, ListenEnd::Finished),
            signal("skipped", "Artist C", 1.0, 5.0, ListenEnd::Interrupted),
        ];
        let favorites = [FavoriteSignal {
            media_id: "favorite".into(),
            title: "Fav".into(),
            artist: "Artist D".into(),
            remote: true,
            added_at: NOW,
        }];
        let profile = Profile::build(&signals, &favorites, NOW, &AffinityConfig::default());
        let order: Vec<_> = profile
            .media()
            .iter()
            .map(|item| item.media_id.as_str())
            .collect();
        assert_eq!(order, ["favorite", "loved", "old", "skipped"]);
        assert!(profile.media_affinity("skipped").unwrap().score < 0.0);
        assert_eq!(profile.media_affinity("skipped").unwrap().skips, 1);
        assert!(profile.media_affinity("favorite").unwrap().favorite);
        assert!(profile.artist_score("ARTIST A") > profile.artist_score("Artist B"));
        assert_eq!(profile.artist_score("Unknown"), 0.0);
        assert!(profile.artist_affinity("Unknown").is_none());
        assert_eq!(profile.config(), &AffinityConfig::default());
    }

    #[test]
    fn seeds_are_positive_and_one_per_artist() {
        let signals = [
            signal("a1", "Artist A", 1.0, 200.0, ListenEnd::Finished),
            signal("a2", "Artist A - Topic", 1.0, 200.0, ListenEnd::Finished),
            signal("b1", "Artist B", 3.0, 200.0, ListenEnd::Finished),
            signal("c1", "Artist C", 1.0, 3.0, ListenEnd::Interrupted),
        ];
        let profile = Profile::build(&signals, &[], NOW, &AffinityConfig::default());
        let seeds: Vec<_> = profile
            .seeds(5)
            .iter()
            .map(|item| item.media_id.clone())
            .collect();
        assert_eq!(seeds.len(), 2);
        assert!(seeds.contains(&"b1".to_owned()));
        assert_eq!(profile.top_artists(5).len(), 2);
    }

    #[test]
    fn autoplay_listens_count_less_than_chosen_ones() {
        let mut chosen = signal("chosen", "A", 0.0, 200.0, ListenEnd::Finished);
        let mut picked = signal("picked", "B", 0.0, 200.0, ListenEnd::Finished);
        picked.source = ListenSource::Autoplay;
        chosen.started_at = NOW;
        let profile = Profile::build(&[chosen, picked], &[], NOW, &AffinityConfig::default());
        assert!(
            profile.media_affinity("chosen").unwrap().score
                > profile.media_affinity("picked").unwrap().score
        );
    }

    #[test]
    fn the_short_term_score_follows_recent_bursts_over_old_habits() {
        let mut signals: Vec<_> = (0..20)
            .map(|_| signal("habit", "Old Band", 60.0, 200.0, ListenEnd::Finished))
            .collect();
        signals
            .extend((0..3).map(|_| signal("burst", "New Band", 1.0, 200.0, ListenEnd::Finished)));
        let profile = Profile::build(&signals, &[], NOW, &AffinityConfig::default());
        let habit = profile.media_affinity("habit").unwrap();
        let burst = profile.media_affinity("burst").unwrap();
        assert!(habit.score > burst.score, "long-term keeps the habit first");
        assert!(burst.short_term > 10.0 * habit.short_term);
        let new_band = profile.artist_affinity("New Band").unwrap();
        assert_eq!(new_band.short_term, burst.short_term);
        let fresh = FavoriteSignal {
            media_id: "fresh".into(),
            title: "Fresh".into(),
            artist: "C".into(),
            remote: false,
            added_at: NOW,
        };
        let profile = Profile::build(&[], &[fresh], NOW, &AffinityConfig::default());
        assert_eq!(profile.media_affinity("fresh").unwrap().short_term, 3.0);
    }
}
