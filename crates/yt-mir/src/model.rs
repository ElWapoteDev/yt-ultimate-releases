//! The Beat This! network in ONNX Runtime, and how every model of this crate
//! is opened: checked against its SHA-256, on the GPU through CoreML (macOS)
//! or DirectML (Windows), on the CPU only when the caller asks for it.

use crate::{
    beats::{self, CHUNK, NO_PREDICTION},
    error::runtime,
    spectrogram::N_MELS,
    MirError, Result, Stopwatch,
};
use ort::{
    ep::ExecutionProviderDispatch,
    session::{builder::GraphOptimizationLevel, Session},
    value::TensorRef,
};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path, sync::OnceLock};

/// File name of the exported model (`models/export_beat_this.py`).
pub const BEAT_MODEL_FILENAME: &str = "beat-this-final0.onnx";
/// SHA-256 of the exported model; any other file is refused.
pub const BEAT_MODEL_SHA256: &str =
    "65d122c994112307303403c3f3e40e3d6f271cb2d7b6790508c1b48d3655b942";
const INPUT: &str = "spect";
const BEAT_OUTPUT: &str = "beat";
const DOWNBEAT_OUTPUT: &str = "downbeat";
/// Bytes read per step while hashing the model file.
const HASH_BLOCK: usize = 1 << 20;

/// Where the network runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Device {
    /// CoreML on the GPU (macOS) or DirectML (Windows). Loading fails rather
    /// than silently falling back to the CPU.
    Gpu,
    /// ONNX Runtime's CPU provider on one thread: tests, and machines whose
    /// GPU cannot run the model.
    Cpu,
}

/// Load the ONNX Runtime library at `library` for this process. Only the
/// first call loads anything; later calls return its result.
pub fn init_runtime(library: &Path) -> Result<()> {
    static RUNTIME: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            ort::init_from(library)
                .map_err(|error| error.to_string())?
                .with_name("yt-mir")
                .commit();
            Ok(())
        })
        .clone()
        .map_err(MirError::Runtime)
}

/// A loaded beat model, ready to predict.
pub struct BeatModel {
    session: Session,
    device: Device,
}

impl BeatModel {
    /// Check `path` against [`BEAT_MODEL_SHA256`] and load it on `device`.
    /// `cache` keeps CoreML's compiled model so later loads skip compiling it
    /// (macOS; ignored elsewhere). Needs [`init_runtime`] first.
    pub fn load(path: &Path, device: Device, cache: Option<&Path>) -> Result<Self> {
        let session = open(path, BEAT_MODEL_SHA256, "beat model", device, cache)?;
        Ok(Self { session, device })
    }

    /// Where this model runs.
    pub fn device(&self) -> Device {
        self.device
    }

    /// Beat and downbeat logits for every frame of `spectrogram`
    /// (`frames × N_MELS`), chunked and merged like the reference. Laps
    /// `watch` after every chunk.
    pub(crate) fn predict(
        &mut self,
        spectrogram: &[f32],
        frames: usize,
        watch: &mut Stopwatch<'_>,
    ) -> Result<(Vec<f32>, Vec<f32>)> {
        let mut beat = vec![NO_PREDICTION; frames];
        let mut downbeat = vec![NO_PREDICTION; frames];
        let mut chunk = vec![0.0_f32; CHUNK * N_MELS];
        for start in beats::chunk_starts(frames).into_iter().rev() {
            beats::fill_chunk(spectrogram, frames, start, &mut chunk);
            let input = TensorRef::from_array_view(([1_usize, CHUNK, N_MELS], chunk.as_slice()))
                .map_err(runtime)?;
            let outputs = self
                .session
                .run(ort::inputs![INPUT => input])
                .map_err(runtime)?;
            for (name, piece) in [(BEAT_OUTPUT, &mut beat), (DOWNBEAT_OUTPUT, &mut downbeat)] {
                let (_, values) = outputs[name].try_extract_tensor::<f32>().map_err(runtime)?;
                if values.len() != CHUNK {
                    return Err(MirError::Model(format!(
                        "{name} has {} values, expected {CHUNK}",
                        values.len()
                    )));
                }
                beats::aggregate(piece, start, values);
            }
            watch.lap()?;
        }
        Ok((beat, downbeat))
    }
}

/// A session for the model at `path`, which must hash to `sha256` (`what`
/// names it in errors), on `device`, keeping CoreML's compiled model in
/// `cache`.
pub(crate) fn open(
    path: &Path,
    sha256: &str,
    what: &str,
    device: Device,
    cache: Option<&Path>,
) -> Result<Session> {
    verify(path, sha256, what)?;
    let builder = Session::builder()
        .map_err(runtime)?
        .with_optimization_level(GraphOptimizationLevel::All)
        .map_err(runtime)?
        .with_intra_threads(1)
        .map_err(runtime)?;
    let mut builder = match device {
        Device::Gpu => gpu_session(builder, cache)?,
        Device::Cpu => builder,
    };
    builder.commit_from_file(path).map_err(runtime)
}

#[cfg(target_os = "macos")]
fn gpu_session(
    builder: ort::session::builder::SessionBuilder,
    cache: Option<&Path>,
) -> Result<ort::session::builder::SessionBuilder> {
    use ort::ep::coreml::{ComputeUnits, ModelFormat};
    let mut provider = ort::ep::CoreML::default()
        .with_model_format(ModelFormat::MLProgram)
        .with_compute_units(ComputeUnits::CPUAndGPU)
        .with_static_input_shapes(true);
    if let Some(cache) = cache {
        provider = provider.with_model_cache_dir(cache.display());
    }
    with_provider(builder, provider.build())
}

#[cfg(windows)]
fn gpu_session(
    builder: ort::session::builder::SessionBuilder,
    _cache: Option<&Path>,
) -> Result<ort::session::builder::SessionBuilder> {
    // DirectML runs one graph at a time and cannot use memory patterns.
    let builder = builder
        .with_memory_pattern(false)
        .map_err(runtime)?
        .with_parallel_execution(false)
        .map_err(runtime)?;
    with_provider(builder, ort::ep::DirectML::default().build())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn gpu_session(
    _builder: ort::session::builder::SessionBuilder,
    _cache: Option<&Path>,
) -> Result<ort::session::builder::SessionBuilder> {
    Err(MirError::Runtime(
        "there is no GPU execution provider for this platform".into(),
    ))
}

#[cfg(any(target_os = "macos", windows))]
fn with_provider(
    builder: ort::session::builder::SessionBuilder,
    provider: ExecutionProviderDispatch,
) -> Result<ort::session::builder::SessionBuilder> {
    builder
        .with_execution_providers([provider.error_on_failure()])
        .map_err(runtime)
}

/// Refuse any file whose SHA-256 is not `sha256`.
fn verify(path: &Path, sha256: &str, what: &str) -> Result<()> {
    let unreadable =
        |error: std::io::Error| MirError::Model(format!("{}: {error}", path.display()));
    let mut file = File::open(path).map_err(unreadable)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; HASH_BLOCK];
    loop {
        let read = file.read(&mut buffer).map_err(unreadable)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if digest == sha256 {
        Ok(())
    } else {
        Err(MirError::Model(format!(
            "{} is not the expected {what}",
            path.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_other_than_the_exported_model_are_refused() {
        let file =
            std::env::temp_dir().join(format!("yt-mir-not-a-model-{}.onnx", std::process::id()));
        std::fs::write(&file, b"not a model").unwrap();
        assert!(matches!(
            verify(&file, BEAT_MODEL_SHA256, "beat model"),
            Err(MirError::Model(_))
        ));
        std::fs::remove_file(&file).unwrap();
        assert!(matches!(
            verify(
                Path::new("/nonexistent/model.onnx"),
                BEAT_MODEL_SHA256,
                "beat model"
            ),
            Err(MirError::Model(_))
        ));
    }
}
