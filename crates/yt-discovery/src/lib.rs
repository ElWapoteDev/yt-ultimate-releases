//! Pure listening affinity, taste modelling and recommendation ranking.
//!
//! Inputs are typed facts read by `yt-core` (listening events, favorites,
//! audio features) and candidates fetched by the application; outputs are
//! profiles, embeddings, mixes and orderings. This crate performs no IO,
//! never reads the clock (time is a parameter), draws randomness only from
//! seeds the caller passes, and is deterministic for equal inputs.

mod affinity;
mod calendar;
mod clusters;
mod context;
mod diversity;
mod embedding;
mod explore;
mod listen;
mod moods;
mod ranking;
mod replay;
mod rng;
mod sessions;
mod station;
#[cfg(test)]
mod test_support;
mod weekly;

pub use affinity::{AffinityConfig, ArtistAffinity, FavoriteSignal, MediaAffinity, Profile};
pub use clusters::{taste_clusters, ClusterConfig, TasteCluster};
pub use context::{day_part, day_part_mix, DayPart};
pub use diversity::mmr;
pub use embedding::{EmbeddingConfig, Embeddings};
pub use explore::{ArtistBeliefs, ExploreConfig};
pub use listen::{ListenMeter, ListenOutcome, ListenPolicy};
pub use moods::{mood_membership, mood_mix, mood_point, Mood, MoodPoint};
pub use ranking::{rank, Candidate, RankContext, RankingConfig};
pub use replay::{on_repeat, replay, top_artists_by_time, ReplayEntry};
pub use rng::{iso_week, week_seed, SeededRng};
pub use sessions::{sessions, ListeningSession, SESSION_GAP_SECONDS};
pub use station::station_picks;
pub use weekly::{weekly_discovery, WeeklyConfig};
pub use yt_core::{artist_key, title_key};
