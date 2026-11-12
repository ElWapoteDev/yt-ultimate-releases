use crate::{
    check, invalid,
    remap::Soxr,
    wav::{Spec, Writer},
    CancellationToken, Result,
};
use rodio::Source;
use std::path::Path;

#[derive(Clone, Debug, serde::Serialize)]
pub struct PreparationReport {
    pub input_sample_rate: u32,
    pub input_channels: u16,
    pub input_frames: u64,
    pub output_frames: u64,
    pub sample_rate: u32,
}

/// Decode locally and prepare stereo 44.1 kHz float32 WAV for the vocal model.
/// SoXR HQ performs rate conversion; native 44.1 kHz input is not resampled.
/// Conventional 3.0, quad, 5.0, 5.1, 6.1 and 7.1 layouts use a normalized
/// center/surround stereo matrix. LFE is omitted, as in standard stereo downmix.
pub fn prepare_source(
    input: impl AsRef<Path>,
    output: impl AsRef<Path>,
    cancel: &CancellationToken,
) -> Result<PreparationReport> {
    check(cancel)?;
    let mut decoder = yt_media::decode_file(input).map_err(|e| invalid(e.to_string()))?;
    let rate = decoder.sample_rate();
    let channels = decoder.channels();
    if !(1..=8).contains(&channels) || !(8000..=384000).contains(&rate) {
        return Err(invalid(
            "La fuente tiene un formato de audio no compatible.",
        ));
    }
    let mut resampler = if rate != 44100 {
        Some(Soxr::fixed(rate as f64 / 44100.0, 2)?)
    } else {
        None
    };
    let mut writer = Writer::float32(
        output,
        Spec {
            rate: 44100,
            channels: 2,
            frames: 0,
        },
    )?;
    let mut input_frames = 0;
    let mut output_frames = 0;
    loop {
        check(cancel)?;
        let samples: Vec<f64> = decoder
            .by_ref()
            .take(8192 * channels as usize)
            .map(f64::from)
            .collect();
        if samples.iter().any(|v| !v.is_finite())
            || !samples.len().is_multiple_of(channels as usize)
        {
            return Err(invalid("La fuente contiene audio incompleto o no válido."));
        }
        if samples.is_empty() {
            break;
        }
        let mut stereo = Vec::with_capacity(samples.len() * 2 / channels as usize);
        for frame in samples.chunks_exact(channels as usize) {
            stereo.extend_from_slice(&downmix(frame));
        }
        input_frames += (samples.len() / channels as usize) as u64;
        let prepared = match resampler.as_mut() {
            Some(resampler) => resampler.chunk(&stereo, false)?,
            None => stereo,
        };
        output_frames += (prepared.len() / 2) as u64;
        writer.write(&prepared)?;
    }
    if let Some(error) = decoder.decoding_error() {
        return Err(invalid(error));
    }
    if let Some(resampler) = resampler.as_mut() {
        let tail = resampler.chunk(&[], true)?;
        output_frames += (tail.len() / 2) as u64;
        writer.write(&tail)?;
    }
    check(cancel)?;
    writer.finish()?;
    Ok(PreparationReport {
        input_sample_rate: rate,
        input_channels: channels,
        input_frames,
        output_frames,
        sample_rate: 44100,
    })
}

fn downmix(frame: &[f64]) -> [f64; 2] {
    let surround = std::f64::consts::FRAC_1_SQRT_2;
    match frame {
        [mono] => [*mono, *mono],
        [left, right] => [*left, *right],
        [left, right, center] => [
            (left + surround * center) / (1.0 + surround),
            (right + surround * center) / (1.0 + surround),
        ],
        [left, right, back_left, back_right] => [
            (left + surround * back_left) / (1.0 + surround),
            (right + surround * back_right) / (1.0 + surround),
        ],
        [left, right, center, back_left, back_right] => [
            (left + surround * (center + back_left)) / (1.0 + 2.0 * surround),
            (right + surround * (center + back_right)) / (1.0 + 2.0 * surround),
        ],
        [left, right, center, _, back_left, back_right] => [
            (left + surround * (center + back_left)) / (1.0 + 2.0 * surround),
            (right + surround * (center + back_right)) / (1.0 + 2.0 * surround),
        ],
        [left, right, center, _, back, side_left, side_right] => [
            (left + surround * (center + side_left) + 0.5 * back) / (1.5 + 2.0 * surround),
            (right + surround * (center + side_right) + 0.5 * back) / (1.5 + 2.0 * surround),
        ],
        [left, right, center, _, back_left, back_right, side_left, side_right] => [
            (left + surround * (center + back_left + side_left)) / (1.0 + 3.0 * surround),
            (right + surround * (center + back_right + side_right)) / (1.0 + 3.0 * surround),
        ],
        _ => unreachable!("channel count validated"),
    }
}
