//! Chunking and peak picking ported from Beat This! (`split_piece`,
//! `aggregate_prediction` with "keep_first", and the "minimal"
//! postprocessor), so beats match the reference implementation frame for frame.

use crate::spectrogram::N_MELS;

/// Frames per model call; the ONNX graph has this static length.
pub(crate) const CHUNK: usize = 1_500;
/// Frames discarded at each edge of a chunk's predictions.
pub(crate) const BORDER: usize = 6;
/// Logit of frames no chunk predicted (probability zero).
pub(crate) const NO_PREDICTION: f32 = -1_000.0;
/// A peak is the maximum within this many frames on either side (±60 ms).
const PEAK_RADIUS: usize = 3;
/// Peaks need a logit above this (probability above one half).
const PEAK_LOGIT: f32 = 0.0;
/// Peaks this many frames apart or closer merge into their mean.
const MERGE_WIDTH: f64 = 1.0;

/// First frame of each chunk, possibly negative (left zero padding).
pub(crate) fn chunk_starts(frames: usize) -> Vec<isize> {
    let stride = (CHUNK - 2 * BORDER) as isize;
    let end = frames as isize - BORDER as isize;
    let mut starts: Vec<isize> = (0..)
        .map(|index| -(BORDER as isize) + index * stride)
        .take_while(|start| *start < end)
        .collect();
    if frames > CHUNK - 2 * BORDER {
        if let Some(last) = starts.last_mut() {
            *last = frames as isize - (CHUNK - BORDER) as isize;
        }
    }
    starts
}

/// Copy the spectrogram rows of the chunk starting at `start` into `chunk`
/// (`CHUNK × N_MELS`), zero-filling outside the piece.
pub(crate) fn fill_chunk(spectrogram: &[f32], frames: usize, start: isize, chunk: &mut [f32]) {
    chunk.fill(0.0);
    let first = start.max(0) as usize;
    let last = ((start + CHUNK as isize) as usize).min(frames);
    let offset = (first as isize - start) as usize;
    if first < last {
        chunk[offset * N_MELS..(offset + last - first) * N_MELS]
            .copy_from_slice(&spectrogram[first * N_MELS..last * N_MELS]);
    }
}

/// Write the chunk prediction for `start`, minus its borders, into `piece`.
/// Called in reverse chunk order, so earlier chunks win where they overlap.
pub(crate) fn aggregate(piece: &mut [f32], start: isize, prediction: &[f32]) {
    let from = start + BORDER as isize;
    let to = (start + (CHUNK - BORDER) as isize).min(piece.len() as isize);
    for frame in from.max(0)..to {
        piece[frame as usize] = prediction[(frame - start) as usize];
    }
}

/// Frame positions of the peaks: local maxima within ±[`PEAK_RADIUS`] above
/// [`PEAK_LOGIT`], with adjacent ones merged into their mean.
pub(crate) fn pick_peaks(logits: &[f32]) -> Vec<f64> {
    let mut peaks = Vec::new();
    let mut group: Option<(f64, f64)> = None;
    for (frame, &value) in logits.iter().enumerate() {
        let from = frame.saturating_sub(PEAK_RADIUS);
        let to = (frame + PEAK_RADIUS + 1).min(logits.len());
        let local_max = logits[from..to]
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        if value != local_max || value <= PEAK_LOGIT {
            continue;
        }
        let position = frame as f64;
        group = Some(match group {
            Some((mean, count)) if position - mean <= MERGE_WIDTH => {
                let count = count + 1.0;
                (mean + (position - mean) / count, count)
            }
            Some((mean, _)) => {
                peaks.push(mean);
                (position, 1.0)
            }
            None => (position, 1.0),
        });
    }
    if let Some((mean, _)) = group {
        peaks.push(mean);
    }
    peaks
}

/// Move each downbeat to its nearest beat (the first on a tie) and return the
/// distinct beat indices in order.
pub(crate) fn snap_downbeats(beats: &[f64], downbeats: &[f64]) -> Vec<u32> {
    if beats.is_empty() {
        return Vec::new();
    }
    let mut indices: Vec<u32> = downbeats
        .iter()
        .map(|downbeat| {
            beats
                .iter()
                .enumerate()
                .fold((0, f64::INFINITY), |best, (index, beat)| {
                    let distance = (beat - downbeat).abs();
                    if distance < best.1 {
                        (index, distance)
                    } else {
                        best
                    }
                })
                .0 as u32
        })
        .collect();
    indices.sort_unstable();
    indices.dedup();
    indices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_starts_match_the_reference_formula() {
        // np.arange(-6, frames - 6, 1488), last moved to frames - 1494.
        assert_eq!(chunk_starts(1_000), vec![-6]);
        assert_eq!(chunk_starts(1_495), vec![-6, 1]);
        assert_eq!(chunk_starts(3_000), vec![-6, 1_482, 1_506]);
        assert_eq!(
            chunk_starts(10_500),
            vec![-6, 1_482, 2_970, 4_458, 5_946, 7_434, 8_922, 9_006]
        );
    }

    #[test]
    fn chunks_pad_with_zeros_and_aggregation_keeps_the_first_chunk() {
        let frames = 3_000;
        let spectrogram: Vec<f32> = (0..frames * N_MELS)
            .map(|i| (i / N_MELS) as f32 + 1.0)
            .collect();
        let mut chunk = vec![0.0; CHUNK * N_MELS];
        fill_chunk(&spectrogram, frames, -6, &mut chunk);
        assert!(chunk[..6 * N_MELS].iter().all(|v| *v == 0.0));
        assert_eq!(chunk[6 * N_MELS], 1.0);
        fill_chunk(&spectrogram, frames, 1_506, &mut chunk);
        assert_eq!(chunk[(CHUNK - 7) * N_MELS], frames as f32);
        assert!(chunk[(CHUNK - 6) * N_MELS..].iter().all(|v| *v == 0.0));

        // Each chunk predicts "frame + start / 1000", so a value names its chunk.
        let tagged = |frame: usize, start: isize| frame as f32 + start as f32 * 0.001;
        let mut piece = vec![NO_PREDICTION; frames];
        for start in chunk_starts(frames).into_iter().rev() {
            let prediction: Vec<f32> = (0..CHUNK)
                .map(|i| tagged((start + i as isize).max(0) as usize, start))
                .collect();
            aggregate(&mut piece, start, &prediction);
        }
        assert_eq!(
            piece[0],
            tagged(0, -6),
            "frame 0 comes from the first chunk"
        );
        assert_eq!(piece[1_490], tagged(1_490, 1_482));
        assert_eq!(
            piece[2_000],
            tagged(2_000, 1_482),
            "overlaps keep the earlier chunk"
        );
        assert_eq!(piece[2_990], tagged(2_990, 1_506));
        assert!(piece.iter().all(|v| *v > NO_PREDICTION));
    }

    #[test]
    fn peaks_are_local_maxima_above_zero_with_neighbours_merged() {
        let mut logits = vec![-5.0_f32; 40];
        logits[5] = 2.0;
        logits[12] = 1.0;
        logits[13] = 1.0;
        logits[20] = -0.5;
        logits[30] = 3.0;
        logits[32] = 2.5;
        assert_eq!(pick_peaks(&logits), vec![5.0, 12.5, 30.0]);
        assert!(pick_peaks(&[-1.0; 10]).is_empty());
    }

    #[test]
    fn downbeats_snap_to_the_nearest_beat_once() {
        let beats = [10.0, 35.0, 60.0, 85.0];
        assert_eq!(snap_downbeats(&beats, &[11.0, 59.0, 61.0]), vec![0, 2]);
        assert!(snap_downbeats(&[], &[1.0]).is_empty());
    }
}
