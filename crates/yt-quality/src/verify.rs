use crate::{
    check, estimate_true_peak, invalid,
    wav::{Reader, Writer},
    CancellationToken, Result,
};
use rodio::Source;
use std::{fs::File, io::Read, path::Path};

#[derive(Clone, Debug, serde::Serialize)]
pub struct VerificationReport {
    pub frames: u64,
    pub sample_rate: u32,
    pub channels: usize,
    pub bit_depth: u8,
    pub max_abs_error: f64,
    pub tolerance: f64,
    pub true_peak_estimate_dbtp: Option<f64>,
}

/// Audit a final FLAC against the processed float64 reference, including exact
/// format/frame count, <= one PCM24 step error and <= -1 dBTP estimated peak.
/// Call before publishing the encoded candidate to the user's library.
pub fn verify_final_flac(
    reference: impl AsRef<Path>,
    encoded: impl AsRef<Path>,
    cancel: &CancellationToken,
) -> Result<VerificationReport> {
    check(cancel)?;
    let encoded = encoded.as_ref();
    let mut reference = Reader::open(reference)?;
    let spec = reference.spec;
    let mut header = [0_u8; 42];
    File::open(encoded)?.read_exact(&mut header)?;
    if &header[..4] != b"fLaC" || header[4] & 0x7f != 0 || header[5..8] != [0, 0, 34] {
        return Err(invalid("La salida no es FLAC con STREAMINFO válido."));
    }
    let packed = u64::from_be_bytes(header[18..26].try_into().unwrap());
    let rate = (packed >> 44) as u32;
    let channels = ((packed >> 41) & 7) as usize + 1;
    let bits = ((packed >> 36) & 31) + 1;
    let frames = packed & 0xfffffffff;
    if (rate, channels, frames) != (spec.rate, spec.channels, spec.frames) || bits != 24 {
        return Err(invalid(
            "El FLAC final no conserva el formato de 24 bits y la duración.",
        ));
    }
    let mut decoded = yt_media::decode_file(encoded).map_err(|e| invalid(e.to_string()))?;
    if (decoded.sample_rate(), decoded.channels() as usize) != (spec.rate, spec.channels) {
        return Err(invalid("El decodificador no confirmó el formato final."));
    }
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("decoded.wav");
    let mut writer = Writer::new(&path, spec)?;
    let tolerance = 2_f64.powi(-23);
    let mut maximum_error = 0.0_f64;
    loop {
        check(cancel)?;
        let source = reference.read(65536)?;
        if source.is_empty() {
            break;
        }
        let samples: Vec<f64> = decoded.by_ref().take(source.len()).map(f64::from).collect();
        if samples.len() != source.len() {
            return Err(invalid("El FLAC está incompleto."));
        }
        for (actual, expected) in samples.iter().zip(source) {
            if !actual.is_finite() {
                return Err(invalid("El FLAC contiene muestras inválidas."));
            }
            maximum_error = maximum_error.max((actual - expected).abs());
        }
        if maximum_error > tolerance {
            return Err(invalid("La exportación alteró la mezcla procesada."));
        }
        writer.write(&samples)?;
    }
    if decoded.next().is_some() || decoded.decoding_error().is_some() {
        return Err(invalid(
            "El FLAC tiene muestras adicionales o errores de decodificación.",
        ));
    }
    writer.finish()?;
    let peak = estimate_true_peak(path, cancel)?;
    if peak.peak_dbtp.is_some_and(|db| db > -1.0) {
        return Err(invalid(
            "El FLAC final excede el margen de pico de −1 dBTP.",
        ));
    }
    Ok(VerificationReport {
        frames: spec.frames,
        sample_rate: spec.rate,
        channels: spec.channels,
        bit_depth: 24,
        max_abs_error: maximum_error,
        tolerance,
        true_peak_estimate_dbtp: peak.peak_dbtp,
    })
}
