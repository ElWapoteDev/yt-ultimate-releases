//! The DJ's transitions as pure functions of the two songs' analyses: where a
//! mix goes and which technique it uses ([`plan`]), what each deck does over
//! its seconds ([`Lanes`]), the per-deck DSP that plays it
//! ([`DeckProcessor`]), and a first evaluation that rejects mixes whose beats
//! drift apart or whose bass plays twice ([`MixReport`]), and where an AI
//! fusion goes ([`Fusion`]). No files or clock,
//! and only seeded randomness. See `README.md` and `docs/AI_DJ.md` §5.5.

mod automation;
mod dsp;
mod error;
mod evaluate;
mod fusion;
mod grid;
mod harmony;
mod plan;

pub use automation::{Curve, DeckParams, Effects, Lanes};
pub use dsp::{
    DeckProcessor, CONTROL_FRAMES, FILTER_FLOOR_HZ, FILTER_OCTAVES, HIGH_SHELF_HZ, LOW_SHELF_HZ,
    MAX_CHANNELS, MID_PEAK_HZ, MIN_EQ_DB,
};
pub use error::PlanError;
pub use evaluate::{MixReport, MAX_BEAT_ERROR_MS};
pub use fusion::{
    Fusion, FUSION_CLASH_STRENGTH, FUSION_STRENGTH, MAX_FUSION_CONTEXT_SECONDS,
    MAX_FUSION_SEAM_SECONDS, MAX_FUSION_SECONDS,
};
pub use harmony::{
    camelot, key_distance, transposed, vocal_shift, COMPATIBLE_KEYS, MAX_VOCAL_SHIFT,
};
pub use plan::{
    plan, MixPlan, MixRequest, Technique, VocalShift, FUSION_MAX_STRETCH, MASHUP_MAX_STRETCH,
    MAX_STRETCH, STEM_MARGIN_SECONDS,
};
