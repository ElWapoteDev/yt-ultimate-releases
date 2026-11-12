//! Pure search for YT Ultimate: text folding, BM25F relevance with
//! as-you-type completion and typo tolerance, query auto-completion, rank
//! fusion, personal re-ranking and the choice of a top result.
//!
//! The application passes typed, already validated values (library tracks,
//! online results it fetched, the user's search history and affinities) and
//! renders the orderings this crate returns. The crate performs no IO, never
//! reads the clock (time is a parameter), uses no randomness, and returns
//! equal output for equal input.

#![warn(missing_docs)]

mod bm25;
mod complete;
mod fuse;
mod fuzzy;
mod intent;
mod normalize;

pub use bm25::{Bm25Config, Document, Field, Hit, Index};
pub use complete::{complete, CompletionConfig, HistoryQuery, Suggestion, SuggestionSource};
pub use fuse::{personalize, reciprocal_rank_fusion, DEFAULT_RRF_K};
pub use fuzzy::{
    edit_distance_within, trigram_similarity, typo_budget, ONE_TYPO_MIN_CHARS, TWO_TYPOS_MIN_CHARS,
};
pub use intent::{top_result, TopKind};
pub use normalize::{fold, tokens};
