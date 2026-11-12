//! Native processing stages ported from the legacy quality_audio contract.
mod eq;
mod mix;
mod peak;
mod prepare;
mod remap;
#[cfg(test)]
mod tests;
mod verify;
mod wav;

pub use eq::{dynamic_instrumental_eq, EqReport};
pub use mix::{mix_preserving_vocals, MixReport};
pub use peak::{estimate_true_peak, PeakReport};
pub use prepare::{prepare_source, PreparationReport};
pub use remap::{shared_smooth_remap, RemapReport};
use std::path::Path;
pub use tokio_util::sync::CancellationToken;
pub use verify::{verify_final_flac, VerificationReport};

#[derive(Debug, thiserror::Error)]
pub enum QualityError {
    #[error("{0}")]
    Invalid(String),
    #[error("La operación fue cancelada.")]
    Cancelled,
    #[error("El destino ya existe y no se ha reemplazado.")]
    DestinationExists,
    #[error("No se pudo procesar el archivo: {0}")]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, QualityError>;

pub(crate) fn check(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(QualityError::Cancelled)
    } else {
        Ok(())
    }
}
pub(crate) fn invalid(message: impl Into<String>) -> QualityError {
    QualityError::Invalid(message.into())
}

#[derive(Clone, Copy, Debug)]
pub struct QualityProgress {
    pub fraction: f64,
    pub stage: &'static str,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct QualityReport {
    pub instrumental: EqReport,
    pub mix: MixReport,
    pub shared: RemapReport,
    pub pre_remap_pcm24_error: f64,
}

/// Run the DSP stages on prepared, synchronized vocal/instrumental WAVs.
/// Output is a float64 WAV reference. Encode and audit the final FLAC separately.
pub fn process_stems(
    vocals: impl AsRef<Path>,
    instrumental: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    seed: &str,
    cancel: &CancellationToken,
    progress: impl Fn(QualityProgress),
) -> Result<QualityReport> {
    check(cancel)?;
    let destination = destination.as_ref();
    if destination.exists() {
        return Err(QualityError::DestinationExists);
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temporary = tempfile::Builder::new()
        .prefix(".yt-quality-")
        .tempdir_in(parent)?;
    progress(QualityProgress {
        fraction: 0.0,
        stage: "instrumental_eq",
    });
    let eq_path = temporary.path().join("instrumental.wav");
    let instrumental = dynamic_instrumental_eq(instrumental, &eq_path, seed, 6.0, cancel)?;
    progress(QualityProgress {
        fraction: 0.35,
        stage: "mixing",
    });
    let mixed_path = temporary.path().join("mixed.wav");
    let mix = mix_preserving_vocals(&vocals, &eq_path, &mixed_path, cancel)?;
    let quantized_path = temporary.path().join("before-remap.wav");
    let pre_remap_pcm24_error = mix::quantize_mix_reference(
        vocals.as_ref(),
        &eq_path,
        &mixed_path,
        &quantized_path,
        mix.instrumental_gain,
        cancel,
    )?;
    progress(QualityProgress {
        fraction: 0.5,
        stage: "shared_remap",
    });
    let shared = shared_smooth_remap(quantized_path, destination, cancel)?;
    progress(QualityProgress {
        fraction: 1.0,
        stage: "complete",
    });
    Ok(QualityReport {
        instrumental,
        mix,
        shared,
        pre_remap_pcm24_error,
    })
}
