//! The beat grid a mix aligns to.
//!
//! The analyzer reports every beat on one of its spectrogram frames, 20 ms
//! apart, so two songs lined up by those positions alone can sit a frame apart
//! and drift over the bars of a mix. Music keeps a steadier beat than that: a
//! straight grid fitted to the beats around the mix tells where they fall, and
//! how long they last, far more precisely than any one of them.

/// Fewest beats a grid is fitted to.
const MIN_FIT_BEATS: usize = 8;
/// Beats beyond the mix, on each side, that a grid is also fitted to: three
/// times the beats give about five times the precision of the period.
pub(crate) const FIT_MARGIN_BEATS: usize = 16;
/// How far an interval may lie from the median one, as a share of it, in beats
/// that follow each other: a missed or an extra beat lies further.
const REGULAR_INTERVAL: f64 = 0.25;

/// A straight beat grid, in frames.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Grid {
    /// Where the beat it was anchored on falls.
    pub origin: f64,
    /// Frames from one beat to the next.
    pub period: f64,
}

/// The least-squares grid through `beats[anchor - before..=anchor + after]`
/// (clamped to the beats there are), anchored on beat `anchor`. `None` for
/// fewer than [`MIN_FIT_BEATS`] beats or when they do not follow each other
/// regularly ([`REGULAR_INTERVAL`]): a missed beat would bend the grid.
pub(crate) fn fit(beats: &[u64], anchor: usize, before: usize, after: usize) -> Option<Grid> {
    let first = anchor.saturating_sub(before);
    let last = (anchor + after).min(beats.len().checked_sub(1)?);
    let window = beats.get(first..=last)?;
    if window.len() < MIN_FIT_BEATS {
        return None;
    }
    let mut intervals: Vec<f64> = window
        .windows(2)
        .map(|pair| pair[1].saturating_sub(pair[0]) as f64)
        .collect();
    let unsorted = intervals.clone();
    intervals.sort_by(f64::total_cmp);
    let median = intervals[intervals.len() / 2];
    let regular = median > 0.0
        && unsorted
            .iter()
            .all(|interval| (interval - median).abs() <= median * REGULAR_INTERVAL);
    if !regular {
        return None;
    }
    let count = window.len() as f64;
    let index = |position: usize| (first + position) as f64 - anchor as f64;
    let mean_index = (0..window.len()).map(index).sum::<f64>() / count;
    let mean_frame = window.iter().map(|frame| *frame as f64).sum::<f64>() / count;
    let (mut covariance, mut variance) = (0.0, 0.0);
    for (position, frame) in window.iter().enumerate() {
        let offset = index(position) - mean_index;
        covariance += offset * (*frame as f64 - mean_frame);
        variance += offset * offset;
    }
    let period = covariance / variance;
    (period.is_finite() && period > 0.0).then_some(Grid {
        origin: mean_frame - period * mean_index,
        period,
    })
}

/// [`fit`] with [`FIT_MARGIN_BEATS`] around the `span` beats after `anchor`,
/// or through those beats alone when the ones around them are irregular.
pub(crate) fn fit_around(beats: &[u64], anchor: usize, span: usize) -> Option<Grid> {
    fit(beats, anchor, FIT_MARGIN_BEATS, span + FIT_MARGIN_BEATS)
        .or_else(|| fit(beats, anchor, 0, span))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Beats `period` frames apart from `start`, each rounded to `step`
    /// frames as the analyzer rounds them to its spectrogram frames.
    fn rounded(start: f64, period: f64, count: usize, step: f64) -> Vec<u64> {
        (0..count)
            .map(|beat| (((start + period * beat as f64) / step).round() * step) as u64)
            .collect()
    }

    #[test]
    fn a_grid_finds_the_beat_between_the_analyzer_frames() {
        // 96 BPM at 44.1 kHz on 20 ms frames: every beat is up to 10 ms off.
        let (period, step) = (27_562.5, 882.0);
        let beats = rounded(10_300.0, period, 200, step);
        let worst = beats
            .iter()
            .enumerate()
            .map(|(beat, frame)| (*frame as f64 - (10_300.0 + period * beat as f64)).abs())
            .fold(0.0, f64::max);
        assert!(worst > 300.0, "the rounding moves beats: {worst}");
        let grid = fit_around(&beats, 100, 32).unwrap();
        assert!(
            (grid.period - period).abs() / period < 2e-4,
            "{}",
            grid.period
        );
        let anchor = 10_300.0 + period * 100.0;
        assert!(
            (grid.origin - anchor).abs() < 100.0,
            "within 2 ms of the beat: {} against {anchor}",
            grid.origin
        );
    }

    #[test]
    fn a_missed_beat_is_not_fitted_through() {
        let mut beats = rounded(0.0, 22_050.0, 80, 1.0);
        let grid = fit(&beats, 40, 16, 16).unwrap();
        assert!((grid.period - 22_050.0).abs() < 1e-6 && (grid.origin - 882_000.0).abs() < 1e-6);
        beats.remove(50);
        assert_eq!(fit(&beats, 40, 16, 16), None);
        assert!(
            fit_around(&beats, 30, 16).is_some(),
            "the beats of the mix alone are regular"
        );
        assert_eq!(fit(&beats, 3, 0, 4), None, "too few beats");
        assert_eq!(fit(&[], 0, 4, 4), None);
    }
}
