//! Local comparison of decoded audio, never an identification-service result.
use crate::{check, codec, AudioError, Result};
use rodio::Source;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::Path;
use yt_media::CancellationToken;

#[derive(Debug, Clone, Serialize)]
pub struct AudioFingerprint {
    pub pcm_sha256: String,
    pub fingerprint_sha256: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: Vec<u32>,
}

/// Hash all decoded native-rate PCM16 samples and fingerprint the first 120 s.
/// The digest includes the rate/channel layout; it is not the legacy resampled
/// FFmpeg digest. Chromaprint preset_test2 corresponds to fpcalc -algorithm 2.
pub fn fingerprint(path: &Path, cancel: &CancellationToken) -> Result<AudioFingerprint> {
    check(cancel)?;
    let mut source = yt_media::decode_file(path).map_err(codec)?;
    let rate = source.sample_rate();
    let channels = source.channels();
    let mut printer =
        rusty_chromaprint::Fingerprinter::new(&rusty_chromaprint::Configuration::preset_test2());
    printer.start(rate, channels as u32).map_err(codec)?;
    let mut hash = Sha256::new();
    hash.update(rate.to_le_bytes());
    hash.update(channels.to_le_bytes());
    let limit = 120 * rate as usize * channels as usize;
    let mut samples = 0usize;
    loop {
        check(cancel)?;
        let floats = source
            .by_ref()
            .take(4096 * channels as usize)
            .collect::<Vec<_>>();
        if floats.is_empty() {
            break;
        }
        if floats.iter().any(|s| !s.is_finite()) {
            return Err(AudioError::Codec(
                "El audio contiene muestras inválidas".into(),
            ));
        }
        let pcm = floats
            .iter()
            .map(|s| (*s as f64 * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
            .collect::<Vec<_>>();
        for sample in &pcm {
            hash.update(sample.to_le_bytes());
        }
        if samples < limit {
            printer.consume(&pcm[..pcm.len().min(limit - samples)]);
        }
        samples += pcm.len();
    }
    if let Some(error) = source.decoding_error() {
        return Err(AudioError::Codec(error));
    }
    if samples == 0 {
        return Err(AudioError::Codec("La fuente no contiene audio".into()));
    }
    printer.finish();
    let frames = printer.fingerprint().to_vec();
    let mut acoustic_hash = Sha256::new();
    for frame in &frames {
        acoustic_hash.update(frame.to_le_bytes());
    }
    Ok(AudioFingerprint {
        pcm_sha256: format!("{:x}", hash.finalize()),
        fingerprint_sha256: format!("{:x}", acoustic_hash.finalize()),
        sample_rate: rate,
        channels,
        frames,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct Alignment {
    pub offset_frames: i32,
    pub compared_frames: usize,
    pub bit_similarity: f64,
    pub exact_frame_fraction: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FrameComparison {
    pub metric: &'static str,
    pub bits_per_frame: usize,
    pub max_offset_frames: i32,
    pub min_overlap_fraction: f64,
    pub offset_convention: &'static str,
    pub aligned: Alignment,
    pub best_offset: Alignment,
    pub min_compared_frames: usize,
}

pub fn compare_fingerprint_frames(source: &[u32], output: &[u32]) -> Result<FrameComparison> {
    let shortest = source.len().min(output.len());
    if shortest < 16 {
        return Err(AudioError::Codec(
            "Audio insuficiente para comparar las huellas".into(),
        ));
    }
    let minimum = 16.max((shortest as f64 * 0.8).ceil() as usize);
    let at = |offset: i32| {
        let start_a = (-offset).max(0) as usize;
        let start_b = offset.max(0) as usize;
        let count = source
            .len()
            .saturating_sub(start_a)
            .min(output.len().saturating_sub(start_b));
        let (differences, exact) = (0..count).fold((0u64, 0usize), |(bits, exact), i| {
            let difference = source[start_a + i] ^ output[start_b + i];
            (
                bits + difference.count_ones() as u64,
                exact + usize::from(difference == 0),
            )
        });
        Alignment {
            offset_frames: offset,
            compared_frames: count,
            bit_similarity: 1.0 - differences as f64 / (32 * count) as f64,
            exact_frame_fraction: exact as f64 / count as f64,
        }
    };
    let aligned = at(0);
    let mut best = aligned.clone();
    for offset in -16i32..=16 {
        let count = source
            .len()
            .saturating_sub((-offset).max(0) as usize)
            .min(output.len().saturating_sub(offset.max(0) as usize));
        if offset == 0 || count < minimum {
            continue;
        }
        let candidate = at(offset);
        let ordering = candidate
            .bit_similarity
            .total_cmp(&best.bit_similarity)
            .then(candidate.compared_frames.cmp(&best.compared_frames))
            .then(best.offset_frames.abs().cmp(&candidate.offset_frames.abs()))
            .then(best.offset_frames.cmp(&candidate.offset_frames));
        if ordering.is_gt() {
            best = candidate;
        }
    }
    Ok(FrameComparison {
        metric: "chromaprint_bit_agreement_v1",
        bits_per_frame: 32,
        max_offset_frames: 16,
        min_overlap_fraction: 0.8,
        offset_convention: "output_index_minus_source_index",
        aligned,
        best_offset: best,
        min_compared_frames: minimum,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioComparison {
    pub pcm_changed: bool,
    pub algorithm: &'static str,
    pub recognition_tested: bool,
    pub source: AudioFingerprint,
    pub output: AudioFingerprint,
    pub local_comparison: Option<FrameComparison>,
}

pub fn compare_audio(
    source: &Path,
    output: &Path,
    cancel: &CancellationToken,
) -> Result<AudioComparison> {
    let source = fingerprint(source, cancel)?;
    let output = fingerprint(output, cancel)?;
    let local_comparison = if source.frames.len().min(output.frames.len()) >= 16 {
        Some(compare_fingerprint_frames(&source.frames, &output.frames)?)
    } else {
        None
    };
    Ok(AudioComparison {
        pcm_changed: source.pcm_sha256 != output.pcm_sha256,
        algorithm: "chromaprint-2",
        recognition_tested: false,
        source,
        output,
        local_comparison,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offset_alignment_and_short_audio_are_reported_honestly() {
        let frames = (0u32..80)
            .map(|v| v.wrapping_mul(0x9e3779b9))
            .collect::<Vec<_>>();
        let mut delayed = vec![0, 0, 0];
        delayed.extend(&frames);
        let comparison = compare_fingerprint_frames(&frames, &delayed).unwrap();
        assert_eq!(comparison.best_offset.offset_frames, 3);
        assert_eq!(comparison.best_offset.bit_similarity, 1.0);
        assert!(comparison.aligned.bit_similarity < 0.8);
        assert!(compare_fingerprint_frames(&frames[..15], &frames).is_err());
    }
    #[test]
    fn real_fingerprint_is_repeatable_and_not_a_recognition_claim() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audio.wav");
        let mut wave = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 11025,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for i in 0..11025 * 15 {
            let phase = i as f64 / 11025.0;
            wave.write_sample(
                (3000.0
                    * (phase * (440.0 + (phase / 2.0).floor() * 40.0) * std::f64::consts::TAU)
                        .sin()) as i16,
            )
            .unwrap();
        }
        wave.finalize().unwrap();
        let report = compare_audio(&path, &path, &CancellationToken::new()).unwrap();
        assert!(!report.pcm_changed);
        assert!(!report.recognition_tested);
        assert_eq!(report.local_comparison.unwrap().aligned.bit_similarity, 1.0);
    }
}
