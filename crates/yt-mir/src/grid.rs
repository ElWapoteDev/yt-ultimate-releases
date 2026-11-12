//! Meter, tempo and tempo stability from beat and downbeat positions.

use yt_core::{Meter, Subdivision, TrackAnalysis};

/// Steady beats needed in a row to fit the tempo by least squares instead of
/// taking the median interval.
const MIN_FIT_BEATS: usize = 8;
/// A beat divides in three when the onset strength at its thirds exceeds the
/// strength at its middle by this factor.
const TERNARY_MARGIN: f32 = 1.15;
const SECONDS_PER_MINUTE: f64 = 60.0;

/// What the beat positions say about the music's pulse.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Grid {
    pub(crate) meter: Option<Meter>,
    pub(crate) tempo_bpm: Option<f32>,
    pub(crate) stability: f32,
}

/// `beats` in seconds; `downbeats` index `beats`; `onset` is the onset
/// strength per spectrogram frame at `fps` frames per second.
pub(crate) fn grid(beats: &[f64], downbeats: &[u32], onset: &[f32], fps: f64) -> Grid {
    let intervals: Vec<f64> = beats.windows(2).map(|pair| pair[1] - pair[0]).collect();
    let median = median(&intervals);
    let tolerance = f64::from(TrackAnalysis::STABLE_INTERVAL);
    // Beats sit on 1/fps frames, so an interval may be one frame off the true period.
    let resolution = 1.0 / fps;
    let steady = |interval: f64| {
        median.is_some_and(|median| (interval - median).abs() <= tolerance * median + resolution)
    };
    let stability = if intervals.is_empty() {
        0.0
    } else {
        intervals
            .iter()
            .filter(|interval| steady(**interval))
            .count() as f32
            / intervals.len() as f32
    };
    let period = fitted_period(beats, &intervals, &steady).or(median);
    let tempo_bpm = period
        .map(|period| (SECONDS_PER_MINUTE / period) as f32)
        .filter(|bpm| bpm.is_finite() && TrackAnalysis::TEMPO_BPM.contains(bpm));
    let meter = bar_length(downbeats).map(|(beats_per_bar, confidence)| Meter {
        beats_per_bar,
        subdivision: subdivision(beats, onset, fps, &steady),
        confidence,
    });
    Grid {
        meter,
        tempo_bpm,
        stability,
    }
}

fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    Some(if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        0.5 * (sorted[middle - 1] + sorted[middle])
    })
}

/// Beat period from a least-squares line through the longest run of steady
/// beats, which is far more precise than any single interval.
fn fitted_period(beats: &[f64], intervals: &[f64], steady: &impl Fn(f64) -> bool) -> Option<f64> {
    if beats.len() < MIN_FIT_BEATS {
        return None;
    }
    let (mut best, mut run_start) = ((0, 0), 0);
    for (index, interval) in intervals.iter().enumerate() {
        if !steady(*interval) {
            run_start = index + 1;
        } else if index + 1 - run_start > best.1 - best.0 {
            best = (run_start, index + 1);
        }
    }
    let run = &beats[best.0..=best.1];
    if run.len() < MIN_FIT_BEATS {
        return None;
    }
    let count = run.len() as f64;
    let mean_index = (count - 1.0) / 2.0;
    let mean_time = run.iter().sum::<f64>() / count;
    let (mut covariance, mut variance) = (0.0, 0.0);
    for (index, time) in run.iter().enumerate() {
        let offset = index as f64 - mean_index;
        covariance += offset * (time - mean_time);
        variance += offset * offset;
    }
    let period = covariance / variance;
    (period.is_finite() && period > 0.0).then_some(period)
}

/// The most common number of beats between downbeats, within
/// [`TrackAnalysis::BEATS_PER_BAR`], and the share of bars that have it.
fn bar_length(downbeats: &[u32]) -> Option<(u8, f32)> {
    let lengths: Vec<u32> = downbeats.windows(2).map(|pair| pair[1] - pair[0]).collect();
    let range = TrackAnalysis::BEATS_PER_BAR;
    let mut counts = [0_usize; 256];
    for length in &lengths {
        if let Ok(length) = u8::try_from(*length) {
            if range.contains(&length) {
                counts[usize::from(length)] += 1;
            }
        }
    }
    let (length, count) = counts
        .iter()
        .enumerate()
        .fold((0, 0), |best, (length, &count)| {
            if count > best.1 {
                (length, count)
            } else {
                best
            }
        });
    (count > 0).then(|| (length as u8, count as f32 / lengths.len() as f32))
}

/// Compare onset strength at each steady beat's middle with its thirds.
fn subdivision(
    beats: &[f64],
    onset: &[f32],
    fps: f64,
    steady: &impl Fn(f64) -> bool,
) -> Subdivision {
    let at = |seconds: f64| {
        let position = seconds * fps;
        let index = position.floor() as usize;
        match (onset.get(index), onset.get(index + 1)) {
            (Some(left), Some(right)) => {
                let fraction = (position - index as f64) as f32;
                left * (1.0 - fraction) + right * fraction
            }
            _ => 0.0,
        }
    };
    let (mut middle, mut thirds, mut count) = (0.0_f32, 0.0_f32, 0_usize);
    for pair in beats.windows(2) {
        let (start, length) = (pair[0], pair[1] - pair[0]);
        if steady(length) {
            middle += at(start + length / 2.0);
            thirds += 0.5 * (at(start + length / 3.0) + at(start + 2.0 * length / 3.0));
            count += 1;
        }
    }
    if count > 0 && thirds > middle * TERNARY_MARGIN {
        Subdivision::Ternary
    } else {
        Subdivision::Binary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FPS: f64 = 50.0;

    fn steady_beats(bpm: f64, count: usize) -> Vec<f64> {
        (0..count)
            .map(|beat| 0.5 + beat as f64 * 60.0 / bpm)
            .collect()
    }

    /// Onset strength with a pulse on every beat and on each subdivision.
    fn onsets(beats: &[f64], divisions: usize) -> Vec<f32> {
        let mut strength = vec![0.0_f32; ((beats.last().unwrap() + 1.0) * FPS) as usize];
        for pair in beats.windows(2) {
            for part in 0..divisions {
                let time = pair[0] + (pair[1] - pair[0]) * part as f64 / divisions as f64;
                strength[(time * FPS).round() as usize] += if part == 0 { 1.0 } else { 0.6 };
            }
        }
        strength
    }

    #[test]
    fn a_steady_four_four_grid_reports_its_tempo_and_meter() {
        let beats = steady_beats(128.0, 64);
        let downbeats: Vec<u32> = (0..64).step_by(4).collect();
        let grid = grid(&beats, &downbeats, &onsets(&beats, 2), FPS);
        assert!((grid.tempo_bpm.unwrap() - 128.0).abs() < 0.01, "{grid:?}");
        assert_eq!(grid.stability, 1.0);
        let meter = grid.meter.unwrap();
        assert_eq!(
            (meter.beats_per_bar, meter.subdivision, meter.confidence),
            (4, Subdivision::Binary, 1.0)
        );
    }

    #[test]
    fn a_waltz_with_triplets_reads_three_beats_divided_in_three() {
        let beats = steady_beats(148.0, 48);
        let downbeats: Vec<u32> = (0..48).step_by(3).collect();
        let grid = grid(&beats, &downbeats, &onsets(&beats, 3), FPS);
        let meter = grid.meter.unwrap();
        assert_eq!(
            (meter.beats_per_bar, meter.subdivision),
            (3, Subdivision::Ternary)
        );
    }

    #[test]
    fn frame_quantized_beats_still_fit_a_precise_tempo() {
        let beats: Vec<f64> = steady_beats(128.0, 80)
            .iter()
            .map(|t| (t * FPS).round() / FPS)
            .collect();
        let grid = grid(&beats, &[], &[], FPS);
        assert!((grid.tempo_bpm.unwrap() - 128.0).abs() < 0.1, "{grid:?}");
        assert!(grid.meter.is_none());
    }

    #[test]
    fn rubato_lowers_stability_and_too_few_beats_have_no_tempo() {
        let wandering: Vec<f64> = (0..32)
            .map(|beat| beat as f64 * 0.5 + 0.2 * (beat as f64).sin())
            .collect();
        assert!(grid(&wandering, &[], &[], FPS).stability < 0.5);
        let empty = grid(&[1.0, 1.5], &[], &[], FPS);
        assert!(empty.tempo_bpm.is_some() && empty.meter.is_none());
        assert_eq!(grid(&[], &[], &[], FPS).tempo_bpm, None);
    }
}
