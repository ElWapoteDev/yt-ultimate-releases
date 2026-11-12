//! The DJ's compute worker. Heavy work (analysis and vocal measurement now;
//! stems, generation and rendering later) runs in `ytu-worker`, a separate
//! low-priority process, so a GPU driver fault or a native runtime crash never
//! stops playback and the models' memory is returned when the process exits.
//! See `README.md` and `docs/AI_DJ.md` §5.8–5.9.
//!
//! - [`protocol`]: the versioned, framed JSON messages both sides exchange.
//! - [`client`]: what the application uses to run jobs in the worker.
//! - [`governor`]: the power and temperature policy the worker applies.
//! - [`fusion`]: the canvas of an AI fusion of two songs, and the model run
//!   that renders it.

pub mod client;
pub mod fusion;
pub mod governor;
pub mod protocol;

pub use client::{Analyzed, ClientError, Separated, Vocals, WorkerClient, WorkerConfig, IDLE_EXIT};
pub use yt_mir::{Device, ANALYZER_VERSION, BEAT_MODEL_FILENAME, VOCAL_MODEL_FILENAME};

/// Exit status of a worker started with a missing runtime, model or argument.
/// The client then treats the device as unable to run the worker instead of
/// restarting it.
pub const EXIT_CONFIGURATION: i32 = 2;
