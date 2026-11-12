//! First evaluation of a mix, from the analyses and the lanes alone (no
//! audio): whether the beats stay together, how long both songs' bass plays
//! at once, and the loudness step the listener hears.

use crate::{
    dsp::{FILTER_FLOOR_HZ, FILTER_OCTAVES},
    plan::{Match, Song},
    Lanes,
};
use yt_core::TrackAnalysis;

/// Largest distance between the songs' beats in a mix that plays, in
/// milliseconds, once smoothed over [`SMOOTHED_BEATS`].
pub const MAX_BEAT_ERROR_MS: f32 = 10.0;
/// Beats whose distances are averaged before they are judged: each detected
/// beat sits on one of the analyzer's frames, up to 10 ms from where it
/// sounds, so only what stays after averaging two bars is the songs really
/// being apart.
const SMOOTHED_BEATS: usize = 8;
/// Bass level above which a deck's bass counts as playing (−6 dB).
const AUDIBLE_BASS: f32 = 0.5;
/// Frequency that stands for the bass when judging the filters, in Hz.
const BASS_HZ: f32 = 80.0;
/// Step of the lane sampling, in seconds.
const STEP_SECONDS: f32 = 0.05;
/// Fewest beat pairs that make a phase measurement meaningful.
const MIN_BEATS_COMPARED: usize = 4;

/// What the evaluation found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MixReport {
    /// How far the incoming song's beats stray from the outgoing song's
    /// during the overlap, in ms: the largest mean distance over
    /// `SMOOTHED_BEATS` consecutive beats. `None` when the technique does not
    /// align beats or too few beats overlap.
    pub beat_error_ms: Option<f32>,
    /// Seconds during which both songs' bass plays at once.
    pub bass_overlap_seconds: f32,
    /// Loudness step between the outgoing song before the mix and the
    /// incoming one after it, in LU; `None` without loudness measurements.
    pub loudness_step_lu: Option<f32>,
}

impl MixReport {
    /// Whether the mix may play: its beats stay within [`MAX_BEAT_ERROR_MS`]
    /// and the two basslines overlap for at most one beat of `beat_seconds`.
    pub fn passes(&self, beat_seconds: f32) -> bool {
        self.beat_error_ms
            .is_none_or(|error| error <= MAX_BEAT_ERROR_MS)
            && self.bass_overlap_seconds <= beat_seconds
    }
}

/// The inputs of one evaluation.
pub(crate) struct Candidate<'a> {
    pub exit: u64,
    pub entry: u64,
    pub speed: f64,
    pub overlap: f32,
    pub trim_db: f32,
    pub outgoing: &'a Lanes,
    pub incoming: &'a Lanes,
}

/// Evaluate `candidate` between the songs analysed in `outgoing` and
/// `incoming`; `aligned` holds both beat grids and their match when the
/// technique keeps the beats together, and only then is the beat error measured.
pub(crate) fn evaluate(
    candidate: &Candidate<'_>,
    outgoing: &TrackAnalysis,
    incoming: &TrackAnalysis,
    aligned: Option<(&Song<'_>, &Song<'_>, Match)>,
) -> MixReport {
    MixReport {
        beat_error_ms: aligned.and_then(|(outgoing, incoming, matched)| {
            beat_error(candidate, outgoing, incoming, matched)
        }),
        bass_overlap_seconds: bass_overlap(candidate),
        loudness_step_lu: loudness_step(candidate, outgoing, incoming),
    }
}

/// Short-term loudness of `analysis` at `seconds`, in LUFS, unless it is silence.
pub(crate) fn loudness_at(analysis: &TrackAnalysis, seconds: f64) -> Option<f32> {
    let index = (seconds / f64::from(TrackAnalysis::LOUDNESS_STEP_SECONDS)).floor();
    if !index.is_finite() || index < 0.0 {
        return None;
    }
    analysis
        .short_term_lufs
        .get(index as usize)
        .copied()
        .filter(|lufs| *lufs > TrackAnalysis::SILENCE_LUFS)
}

/// Every beat of the incoming song in the overlap against the nearest beat
/// of the outgoing one (with half- or double-time matching only its
/// downbeats, which land on the outgoing song's beats), averaged over every
/// [`SMOOTHED_BEATS`] of them: the largest of those means.
fn beat_error(
    candidate: &Candidate<'_>,
    outgoing: &Song<'_>,
    incoming: &Song<'_>,
    matched: Match,
) -> Option<f32> {
    let overlap = f64::from(candidate.overlap);
    let outgoing_times: Vec<f64> = outgoing
        .analysis
        .beats
        .iter()
        .map(|frame| (*frame as f64 - candidate.exit as f64) / outgoing.rate)
        .skip_while(|seconds| *seconds < -outgoing.beat)
        .take_while(|seconds| *seconds <= overlap + outgoing.beat)
        .collect();
    let incoming_frames: Vec<u64> = if matched.beats_per_beat == 1.0 {
        incoming.analysis.beats.clone()
    } else {
        incoming.bars.clone()
    };
    // The entry may lie a few milliseconds past the detected beat it aligns.
    let slack = (incoming.beat / 2.0 * incoming.rate) as u64;
    let apart: Vec<f64> = incoming_frames
        .iter()
        .filter(|frame| **frame + slack >= candidate.entry)
        .map(|frame| (*frame as f64 - candidate.entry as f64) / incoming.rate / candidate.speed)
        .take_while(|seconds| *seconds <= overlap)
        .filter_map(|seconds| {
            outgoing_times
                .iter()
                .map(|beat| seconds - beat)
                .min_by(|a, b| a.abs().total_cmp(&b.abs()))
        })
        .collect();
    if apart.len() < MIN_BEATS_COMPARED {
        return None;
    }
    // A last group too short to average joins the one before it.
    let groups = (apart.len() / SMOOTHED_BEATS).max(1);
    (0..groups)
        .map(|group| {
            let from = group * SMOOTHED_BEATS;
            let to = if group + 1 == groups {
                apart.len()
            } else {
                from + SMOOTHED_BEATS
            };
            let mean = apart[from..to].iter().sum::<f64>() / (to - from) as f64;
            (mean.abs() * 1_000.0) as f32
        })
        .max_by(f32::total_cmp)
}

/// How much of a deck's bass gets through at `seconds`.
fn bass(lanes: &Lanes, seconds: f32) -> f32 {
    let params = lanes.at(seconds);
    let cutoff = |octaves: f32| FILTER_FLOOR_HZ * 2_f32.powf(octaves.clamp(0.0, FILTER_OCTAVES));
    let highpass_open = cutoff(params.highpass) <= BASS_HZ;
    let lowpass_open = cutoff(params.lowpass) >= BASS_HZ;
    if !(highpass_open && lowpass_open) {
        return 0.0;
    }
    // The vocals carry no bassline: only the rest of a stem-split deck counts.
    params.gain * lanes.instrumental.at(seconds) * 10_f32.powf(params.low_db / 20.0)
}

/// The incoming deck is judged against its own level, without the trim that
/// matches its loudness to the outgoing song.
fn bass_overlap(candidate: &Candidate<'_>) -> f32 {
    let steps = (candidate.overlap / STEP_SECONDS).ceil() as usize;
    let trim = 10_f32.powf(candidate.trim_db / 20.0);
    (0..steps)
        .filter(|step| {
            let seconds = *step as f32 * STEP_SECONDS;
            bass(candidate.outgoing, seconds) > AUDIBLE_BASS
                && bass(candidate.incoming, seconds) / trim > AUDIBLE_BASS
        })
        .count() as f32
        * STEP_SECONDS
}

/// Short-term loudness of the outgoing song just before the exit against the
/// incoming song's at the end of the overlap, with the incoming trim.
fn loudness_step(
    candidate: &Candidate<'_>,
    outgoing: &TrackAnalysis,
    incoming: &TrackAnalysis,
) -> Option<f32> {
    let before = loudness_at(
        outgoing,
        candidate.exit as f64 / f64::from(outgoing.sample_rate),
    )?;
    let after_seconds = candidate.entry as f64 / f64::from(incoming.sample_rate)
        + f64::from(candidate.overlap) * candidate.speed;
    let after = loudness_at(incoming, after_seconds)? + candidate.trim_db;
    Some((after - before).abs())
}
