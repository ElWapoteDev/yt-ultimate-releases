//! Music analysis for the DJ: beats, bars, meter, tempo, key and loudness of
//! one track, measured from its decoded samples. See `README.md`.
//!
//! [`Analyzer::analyze`] runs the whole pipeline: a log-mel spectrogram on the
//! CPU, the Beat This! network through ONNX Runtime ([`BeatModel`], on the GPU
//! unless the caller asks for the CPU), peak picking identical to the
//! reference implementation, the bar grid, meter and tempo, the key from
//! `yt_media::Analyzer`'s pitch-class profile, and EBU R128 loudness. The
//! result is a validated [`TrackAnalysis`]. [`VocalModel::measure`] adds, in a
//! second and slower pass, how loud the vocals are every half second
//! (HTDemucs v4 on the GPU).

mod audio;
mod beats;
mod error;
mod grid;
mod loudness;
mod model;
mod separation;
mod spectrogram;
mod stft;
mod structure;

pub use audio::Pcm;
pub use error::{MirError, Result};
pub use model::{init_runtime, BeatModel, Device, BEAT_MODEL_FILENAME, BEAT_MODEL_SHA256};
pub use separation::{VocalModel, VOCAL_MODEL_FILENAME, VOCAL_MODEL_SHA256};

use audio::MODEL_RATE;
use spectrogram::{LogMel, FPS};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use yt_core::{KeyEstimate, TrackAnalysis};

/// Version written into every [`TrackAnalysis`]. Raise it whenever any step
/// changes its output, so cached analyses are measured again.
pub const ANALYZER_VERSION: u16 = 4;
/// Samples pushed to the key analyzer between two calls to [`Pace::proceed`].
const PACE_SAMPLES: usize = 1 << 16;

/// Decides between pieces of an analysis whether it goes on. A bare flag only
/// cancels; the DJ worker's governor also sleeps here to bound how much of the
/// time the GPU and CPU are busy.
pub trait Pace {
    /// Called after each piece of work, which kept the device busy for `busy`.
    /// Returns `false` to stop the analysis with [`MirError::Cancelled`].
    fn proceed(&self, busy: Duration) -> bool;
}

/// A cancellation flag: the analysis stops once it is set.
impl Pace for AtomicBool {
    fn proceed(&self, _busy: Duration) -> bool {
        !self.load(Ordering::Relaxed)
    }
}

/// Measures the work done since the previous call and asks `pace` whether to
/// go on.
pub(crate) struct Stopwatch<'a> {
    pace: &'a dyn Pace,
    since: Instant,
}

impl<'a> Stopwatch<'a> {
    pub(crate) fn new(pace: &'a dyn Pace) -> Self {
        Self {
            pace,
            since: Instant::now(),
        }
    }

    /// `Err(Cancelled)` when the pace stops the work.
    pub(crate) fn lap(&mut self) -> Result<()> {
        let proceed = self.pace.proceed(self.since.elapsed());
        self.since = Instant::now();
        if proceed {
            Ok(())
        } else {
            Err(MirError::Cancelled)
        }
    }
}

/// Analyzes tracks with one loaded beat model. Blocking and CPU/GPU-bound: run
/// it on a background worker.
pub struct Analyzer {
    model: BeatModel,
    mel: LogMel,
}

impl Analyzer {
    /// An analyzer using `model` for beats and downbeats.
    pub fn new(model: BeatModel) -> Self {
        Self {
            model,
            mel: LogMel::new(),
        }
    }

    /// The loaded beat model.
    pub fn model(&self) -> &BeatModel {
        &self.model
    }

    /// Measure `pcm`. Asks `pace` whether to go on between steps, between
    /// model chunks and while building the key profile, and fails with
    /// [`MirError::Cancelled`] when it says no; fails with
    /// [`MirError::Runtime`] when the model cannot run and with
    /// [`MirError::Core`] if the result breaks a [`TrackAnalysis`] bound.
    pub fn analyze(&mut self, pcm: &Pcm<'_>, pace: &dyn Pace) -> Result<TrackAnalysis> {
        let mut watch = Stopwatch::new(pace);
        watch.lap()?;
        let audio = audio::resample(&pcm.mono(), pcm.sample_rate(), MODEL_RATE)?;
        watch.lap()?;
        let spectrogram = self.mel.compute(&audio);
        let frames = LogMel::frames(audio.len());
        watch.lap()?;
        let (beat_logits, downbeat_logits) =
            self.model.predict(&spectrogram, frames, &mut watch)?;
        let beat_seconds: Vec<f64> = beats::pick_peaks(&beat_logits)
            .iter()
            .map(|frame| frame / FPS)
            .collect();
        let downbeat_seconds: Vec<f64> = beats::pick_peaks(&downbeat_logits)
            .iter()
            .map(|frame| frame / FPS)
            .collect();
        let downbeats = beats::snap_downbeats(&beat_seconds, &downbeat_seconds);
        let onset = spectrogram::onset_strength(&spectrogram, frames);
        let grid = grid::grid(&beat_seconds, &downbeats, &onset, FPS);
        watch.lap()?;
        let key = key(pcm, &mut watch)?;
        let loudness = loudness::measure(pcm)?;
        watch.lap()?;
        let bars: Vec<(f64, f64)> = downbeats
            .windows(2)
            .map(|pair| {
                (
                    beat_seconds[pair[0] as usize],
                    beat_seconds[pair[1] as usize],
                )
            })
            .collect();
        let bar_energy = loudness::bar_energy(&audio, MODEL_RATE, &bars);
        let sections = structure::sections(&spectrogram, frames, FPS, &bars, &bar_energy);
        let duration_frames = pcm.frames() as u64;
        let (beats, downbeats) = source_positions(
            &beat_seconds,
            &downbeats,
            pcm.sample_rate(),
            duration_frames,
        );
        let sections = sections
            .into_iter()
            .filter(|section| (section.start_bar as usize) < downbeats.len())
            .collect();
        let analysis = TrackAnalysis {
            analyzer_version: ANALYZER_VERSION,
            sample_rate: pcm.sample_rate(),
            duration_frames,
            beats,
            downbeats,
            meter: grid.meter,
            tempo_bpm: grid.tempo_bpm,
            tempo_stability: grid.stability,
            key,
            integrated_lufs: loudness.integrated_lufs,
            true_peak_dbtp: loudness.true_peak_dbtp,
            short_term_lufs: loudness.short_term_lufs,
            momentary_lufs: loudness.momentary_lufs,
            bar_energy,
            sections,
            vocal_db: Vec::new(),
        };
        analysis.validate()?;
        Ok(analysis)
    }
}

/// The key of `pcm` from the pitch-class profile `yt_media::Analyzer` builds
/// for the library's audio features, so both agree on what a key is.
fn key(pcm: &Pcm<'_>, watch: &mut Stopwatch<'_>) -> Result<Option<KeyEstimate>> {
    let mut analyzer = yt_media::Analyzer::new(pcm.sample_rate(), pcm.channels());
    for piece in pcm.samples().chunks(PACE_SAMPLES) {
        watch.lap()?;
        for &sample in piece {
            analyzer.push(sample);
        }
    }
    Ok(analyzer.key())
}

/// Beat positions in frames of the source audio, dropping any that rounding
/// puts at or past its end (and the downbeats that pointed at them). Beats
/// that would exceed [`TrackAnalysis::MAX_BEATS`] are noise, not a pulse.
fn source_positions(
    seconds: &[f64],
    downbeats: &[u32],
    rate: u32,
    duration_frames: u64,
) -> (Vec<u64>, Vec<u32>) {
    let beats: Vec<u64> = seconds
        .iter()
        .map(|time| (time * f64::from(rate)).round() as u64)
        .take_while(|frame| *frame < duration_frames)
        .collect();
    if beats.len() > TrackAnalysis::MAX_BEATS {
        return (Vec::new(), Vec::new());
    }
    let downbeats = downbeats
        .iter()
        .copied()
        .filter(|index| (*index as usize) < beats.len())
        .collect();
    (beats, downbeats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_convert_to_source_frames_and_stay_inside_the_track() {
        let (beats, downbeats) =
            source_positions(&[0.5, 1.0, 1.5, 2.0], &[0, 2, 3], 44_100, 44_100 * 2);
        assert_eq!(beats, vec![22_050, 44_100, 66_150]);
        assert_eq!(downbeats, vec![0, 2]);
    }
}
