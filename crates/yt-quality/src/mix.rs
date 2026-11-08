use crate::{
    check, invalid,
    wav::{Reader, Writer},
    CancellationToken, Result,
};
use std::path::Path;

pub(crate) fn quantize_mix_reference(
    vocals: &Path,
    instrumental: &Path,
    mixed: &Path,
    destination: &Path,
    gain: f64,
    cancel: &CancellationToken,
) -> Result<f64> {
    let mut voice = Reader::open(vocals)?;
    let mut music = Reader::open(instrumental)?;
    let mut reference = Reader::open(mixed)?;
    let mut output = Writer::new(destination, reference.spec)?;
    let mut maximum_error = 0.0_f64;
    loop {
        check(cancel)?;
        let mut samples = reference.read(65536)?;
        if samples.is_empty() {
            break;
        }
        let vocals = voice.read(65536)?;
        let music = music.read(65536)?;
        for ((value, vocal), instrumental) in samples.iter_mut().zip(vocals).zip(music) {
            *value = (*value * 8388608.0).round().clamp(-8388608.0, 8388607.0) / 8388608.0;
            maximum_error = maximum_error.max((*value - gain * instrumental - vocal).abs());
        }
        if maximum_error > 2_f64.powi(-23) {
            return Err(invalid("La cuantización previa alteró la voz separada."));
        }
        output.write(&samples)?;
    }
    check(cancel)?;
    output.finish()?;
    Ok(maximum_error)
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct MixReport {
    pub instrumental_gain: f64,
    pub frames: u64,
    pub sample_rate: u32,
    pub channels: usize,
    pub vocal_residual_error: f64,
}

/// Find one instrumental gain that leaves the separated vocal unchanged and
/// prevents sample clipping, then audit the f64 sum before publishing it.
pub fn mix_preserving_vocals(
    vocals: impl AsRef<Path>,
    instrumental: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    cancel: &CancellationToken,
) -> Result<MixReport> {
    check(cancel)?;
    let mut voice = Reader::open(vocals)?;
    let mut music = Reader::open(instrumental)?;
    if voice.spec != music.spec {
        return Err(invalid("Los stems no tienen la misma duración o formato."));
    }
    let mut gain = 1.0_f64;
    loop {
        check(cancel)?;
        let v = voice.read(65536)?;
        if v.is_empty() {
            break;
        }
        let m = music.read(65536)?;
        for (v, m) in v.iter().zip(m) {
            if v.abs() > 1.0 {
                return Err(invalid("La voz separada está saturada."));
            }
            if m > 0.0 {
                gain = gain.min((1.0 - v) / m);
            } else if m < 0.0 {
                gain = gain.min((-1.0 - v) / m);
            }
        }
    }
    if gain < 0.05 {
        return Err(invalid(
            "La voz no deja margen suficiente para unir la instrumental.",
        ));
    }
    if gain < 1.0 {
        gain *= 1.0 - 1e-7;
    }
    voice.seek(0)?;
    music.seek(0)?;
    let mut output = Writer::new(destination, voice.spec)?;
    let mut maximum_error = 0.0_f64;
    loop {
        check(cancel)?;
        let v = voice.read(65536)?;
        if v.is_empty() {
            break;
        }
        let m = music.read(65536)?;
        let mixed: Vec<f64> = v.iter().zip(&m).map(|(v, m)| v + gain * m).collect();
        for ((result, v), m) in mixed.iter().zip(&v).zip(&m) {
            maximum_error = maximum_error.max((result - gain * m - v).abs());
        }
        if maximum_error > 2_f64.powi(-23) {
            return Err(invalid("La mezcla alteró la voz separada."));
        }
        output.write(&mixed)?;
    }
    check(cancel)?;
    output.finish()?;
    Ok(MixReport {
        instrumental_gain: gain,
        frames: voice.spec.frames,
        sample_rate: voice.spec.rate,
        channels: voice.spec.channels,
        vocal_residual_error: maximum_error,
    })
}
