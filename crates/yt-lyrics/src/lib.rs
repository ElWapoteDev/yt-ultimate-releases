//! Pure lyrics catalog matching and karaoke timing.
//!
//! Inputs are typed values from `yt-core`: what the application knows about
//! the song that plays, validated catalog candidates and validated lyrics.
//! Outputs are catalog queries, the chosen lyrics and fill timings for the
//! line being sung. This crate performs no file, network or database access,
//! never reads the clock (positions and elapsed time are parameters) and
//! returns equal output for equal input.

mod karaoke;
mod matching;
mod query;

pub use karaoke::{active_line, line_segments, KaraokeConfig, Segment, SyncClock};
pub use matching::{choose, Chosen, MatchConfig};
pub use query::{queries, SongMetadata, MAX_QUERIES};
