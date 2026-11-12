//! The song's structure: sections that start where its energy and timbre
//! change, found with Foote's novelty on bar-level self-similarity (the method
//! Vande Veire and De Bie's automatic DJ uses), aligned to four-bar phrases
//! and labelled by energy against the song's average.

use crate::spectrogram::N_MELS;
use yt_core::{BandEnergy, Section, TrackAnalysis};

/// Bars on each side of a boundary that the novelty compares.
const KERNEL_BARS: usize = 4;
/// Phrase length the boundaries align to, in bars.
const PHRASE_BARS: usize = 4;
/// Timbre coefficients per bar: the DCT of its mean log-mel spectrum, without
/// the first coefficient (that is loudness, measured apart).
const TIMBRE_COEFFICIENTS: usize = 12;
/// Novelty peaks below this share of the strongest are not boundaries.
const MIN_PEAK: f32 = 0.3;
/// Shortest section, in bars.
const MIN_SECTION_BARS: usize = 4;
/// A peak this many bars from a phrase line still counts as on it.
const PHRASE_TOLERANCE_BARS: usize = 1;

/// The sections of a song whose bars span `bars` (start and end seconds) with
/// band levels `energy`, from its log-mel `spectrogram` (`frames × N_MELS` at
/// `fps`). Empty when the song has too few bars.
pub(crate) fn sections(
    spectrogram: &[f32],
    frames: usize,
    fps: f64,
    bars: &[(f64, f64)],
    energy: &[BandEnergy],
) -> Vec<Section> {
    let count = bars.len().min(energy.len());
    if count < 2 * KERNEL_BARS + MIN_SECTION_BARS {
        return Vec::new();
    }
    let levels: Vec<f32> = energy[..count].iter().map(total_level).collect();
    let timbre = standardized(
        &bars[..count]
            .iter()
            .map(|(start, end)| bar_timbre(spectrogram, frames, fps, *start, *end))
            .collect::<Vec<_>>(),
    );
    let level_novelty = novelty(count, |a, b| (levels[a] - levels[b]).abs());
    let timbre_novelty = novelty(count, |a, b| distance(&timbre[a], &timbre[b]));
    let combined: Vec<f32> = level_novelty
        .iter()
        .zip(&timbre_novelty)
        .map(|(level, timbre)| (level * timbre).sqrt())
        .collect();
    let boundaries = boundaries(&combined);
    let average = levels.iter().sum::<f32>() / count as f32;
    let mut starts = vec![0];
    starts.extend(boundaries);
    starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let end = starts.get(index + 1).copied().unwrap_or(count);
            let level = levels[*start..end].iter().sum::<f32>() / (end - start) as f32;
            Section {
                start_bar: *start as u32,
                high_energy: level > average,
                level_db: level.clamp(
                    *TrackAnalysis::LEVEL_DB.start(),
                    *TrackAnalysis::LEVEL_DB.end(),
                ),
            }
        })
        .collect()
}

/// The level of a bar across its three bands, in dB.
fn total_level(bar: &BandEnergy) -> f32 {
    let power: f32 = [bar.low_db, bar.mid_db, bar.high_db]
        .iter()
        .map(|db| 10_f32.powf(db / 10.0))
        .sum();
    10.0 * power.max(1e-20).log10()
}

/// DCT coefficients 1..=12 of the bar's mean log-mel spectrum.
fn bar_timbre(spectrogram: &[f32], frames: usize, fps: f64, start: f64, end: f64) -> Vec<f32> {
    let first = ((start * fps) as usize).min(frames.saturating_sub(1));
    let last = ((end * fps) as usize).clamp(first + 1, frames);
    let mut mean = [0.0_f32; N_MELS];
    for frame in first..last {
        if let Some(row) = spectrogram.get(frame * N_MELS..(frame + 1) * N_MELS) {
            for (sum, value) in mean.iter_mut().zip(row) {
                *sum += value;
            }
        }
    }
    let span = (last - first) as f32;
    (1..=TIMBRE_COEFFICIENTS)
        .map(|k| {
            mean.iter()
                .enumerate()
                .map(|(band, value)| {
                    value / span
                        * (std::f32::consts::PI * k as f32 * (band as f32 + 0.5) / N_MELS as f32)
                            .cos()
                })
                .sum()
        })
        .collect()
}

/// Every coefficient rescaled to zero mean and unit variance across bars.
fn standardized(rows: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let count = rows.len().max(1) as f32;
    let mut out = rows.to_vec();
    for coefficient in 0..TIMBRE_COEFFICIENTS {
        let mean = rows.iter().map(|row| row[coefficient]).sum::<f32>() / count;
        let variance = rows
            .iter()
            .map(|row| (row[coefficient] - mean).powi(2))
            .sum::<f32>()
            / count;
        let deviation = variance.sqrt().max(1e-6);
        for row in &mut out {
            row[coefficient] = (row[coefficient] - mean) / deviation;
        }
    }
    out
}

fn distance(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).powi(2))
        .sum::<f32>()
        .sqrt()
}

/// Novelty at every bar boundary (index `b` sits between bars `b - 1` and
/// `b`): how much more the bars before and after it differ from each other
/// than among themselves, normalized to the strongest boundary.
fn novelty(count: usize, apart: impl Fn(usize, usize) -> f32) -> Vec<f32> {
    let mut curve = vec![0.0_f32; count + 1];
    for (boundary, value) in curve
        .iter_mut()
        .enumerate()
        .take(count - KERNEL_BARS + 1)
        .skip(KERNEL_BARS)
    {
        let before = boundary - KERNEL_BARS..boundary;
        let after = boundary..boundary + KERNEL_BARS;
        let mut cross = 0.0;
        for a in before.clone() {
            for b in after.clone() {
                cross += apart(a, b);
            }
        }
        let mut within = 0.0;
        for side in [before, after] {
            for a in side.clone() {
                for b in side.clone().filter(|b| *b > a) {
                    within += apart(a, b);
                }
            }
        }
        let pairs_within = (KERNEL_BARS * (KERNEL_BARS - 1)) as f32;
        let cross = cross / (KERNEL_BARS * KERNEL_BARS) as f32;
        *value = (cross - within / pairs_within).max(0.0);
    }
    let strongest = curve.iter().copied().fold(0.0, f32::max);
    if strongest > 0.0 {
        for value in &mut curve {
            *value /= strongest;
        }
    }
    curve
}

/// Section starts: novelty peaks of at least [`MIN_PEAK`], moved onto the
/// four-bar phrase grid whose lines carry the most novelty, at least
/// [`MIN_SECTION_BARS`] apart and from both ends, strongest first, at most
/// `TrackAnalysis::MAX_SECTIONS - 1`.
fn boundaries(novelty: &[f32]) -> Vec<usize> {
    let count = novelty.len() - 1;
    let phase = (0..PHRASE_BARS)
        .max_by(|a, b| {
            let strength =
                |phase: usize| -> f32 { novelty.iter().skip(phase).step_by(PHRASE_BARS).sum() };
            strength(*a).total_cmp(&strength(*b))
        })
        .unwrap_or(0);
    let mut peaks: Vec<(usize, f32)> = (1..count)
        .filter(|bar| {
            novelty[*bar] >= MIN_PEAK
                && novelty[*bar] >= novelty[bar - 1]
                && novelty[*bar] >= novelty[bar + 1]
        })
        .filter_map(|bar| {
            let offset = (bar + PHRASE_BARS - phase) % PHRASE_BARS;
            let snapped = if offset <= PHRASE_TOLERANCE_BARS {
                bar - offset
            } else if PHRASE_BARS - offset <= PHRASE_TOLERANCE_BARS {
                bar + PHRASE_BARS - offset
            } else {
                return None;
            };
            Some((snapped, novelty[bar]))
        })
        .collect();
    peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut chosen: Vec<usize> = Vec::new();
    for (bar, _) in peaks {
        let clear = bar >= MIN_SECTION_BARS
            && bar + MIN_SECTION_BARS <= count
            && chosen
                .iter()
                .all(|other| bar.abs_diff(*other) >= MIN_SECTION_BARS);
        if clear && chosen.len() + 1 < TrackAnalysis::MAX_SECTIONS {
            chosen.push(bar);
        }
    }
    chosen.sort_unstable();
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    const FPS: f64 = 50.0;
    const BAR_SECONDS: f64 = 2.0;

    /// A spectrogram, its frame count, bar spans and band levels.
    type Song = (Vec<f32>, usize, Vec<(f64, f64)>, Vec<BandEnergy>);

    /// A song whose bars are quiet and dark, or loud and bright where `loud`
    /// says so.
    fn song(bars: usize, loud: impl Fn(usize) -> bool) -> Song {
        let frames_per_bar = (BAR_SECONDS * FPS) as usize;
        let frames = bars * frames_per_bar;
        let mut spectrogram = vec![0.0_f32; frames * N_MELS];
        for frame in 0..frames {
            let bar = frame / frames_per_bar;
            for band in 0..N_MELS {
                let tilt = band as f32 / N_MELS as f32;
                spectrogram[frame * N_MELS + band] = if loud(bar) {
                    3.0 + 2.0 * tilt
                } else {
                    2.0 - 1.5 * tilt
                };
            }
        }
        let spans = (0..bars)
            .map(|bar| (bar as f64 * BAR_SECONDS, (bar + 1) as f64 * BAR_SECONDS))
            .collect();
        let energy = (0..bars)
            .map(|bar| {
                let db = if loud(bar) { -14.0 } else { -28.0 };
                BandEnergy {
                    low_db: db,
                    mid_db: db - 2.0,
                    high_db: db - 6.0,
                }
            })
            .collect();
        (spectrogram, frames, spans, energy)
    }

    #[test]
    fn an_intro_a_chorus_and_an_outro_are_found_on_their_bars() {
        let (spectrogram, frames, bars, energy) = song(48, |bar| (16..32).contains(&bar));
        let found = sections(&spectrogram, frames, FPS, &bars, &energy);
        let starts: Vec<u32> = found.iter().map(|section| section.start_bar).collect();
        assert_eq!(starts, vec![0, 16, 32], "{found:?}");
        let energy: Vec<bool> = found.iter().map(|section| section.high_energy).collect();
        assert_eq!(energy, vec![false, true, false]);
        assert!(found[1].level_db > found[0].level_db);
    }

    #[test]
    fn a_boundary_off_the_phrase_grid_moves_onto_it_or_is_dropped() {
        let (spectrogram, frames, bars, energy) = song(40, |bar| bar >= 13);
        let found = sections(&spectrogram, frames, FPS, &bars, &energy);
        let starts: Vec<u32> = found.iter().map(|section| section.start_bar).collect();
        assert!(
            starts == vec![0, 12] || starts == vec![0, 13] || starts == vec![0],
            "{starts:?}"
        );
    }

    #[test]
    fn short_or_uniform_songs_have_one_section_or_none() {
        let (spectrogram, frames, bars, energy) = song(8, |_| false);
        assert!(sections(&spectrogram, frames, FPS, &bars, &energy).is_empty());
        let (spectrogram, frames, bars, energy) = song(40, |_| true);
        let found = sections(&spectrogram, frames, FPS, &bars, &energy);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].start_bar, 0);
    }
}
