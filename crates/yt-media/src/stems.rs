//! CPU inference through LibTorch. No Python interpreter or subprocess is used.
use crate::{decode_file, CancellationToken, MediaError, Result};
use hound::{SampleFormat, WavSpec, WavWriter};
use rodio::{source::UniformSourceIterator, Source};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Once,
};
use tch::{CModule, Device, Tensor};

const RATE: u32 = 44_100;
const BLOCK: usize = 352_800;
const HOP: usize = BLOCK / 4;
static THREADS: Once = Once::new();

#[derive(Clone, Copy, Debug)]
pub struct StemProgress {
    pub processed_frames: u64,
    pub total_frames: Option<u64>,
}

impl StemProgress {
    pub fn fraction(self) -> Option<f64> {
        self.total_frames
            .filter(|n| *n > 0)
            .map(|n| (self.processed_frames as f64 / n as f64).min(1.0))
    }
}

#[derive(Clone, Debug)]
pub struct StemReceipt {
    pub vocals: PathBuf,
    pub instrumental: PathBuf,
    pub frames: u64,
    pub sample_rate: u32,
}

/// The exported BS-RoFormer vocals model, fixed at stereo/44.1 kHz/8 seconds.
/// Loading and inference block; use a worker thread. CPU inference is supported
/// on both desktop targets. The model and matching LibTorch must be installed.
pub struct StemSeparator {
    model: CModule,
}

impl StemSeparator {
    pub fn load(model_path: impl AsRef<Path>) -> Result<Self> {
        THREADS.call_once(|| {
            let available = std::thread::available_parallelism().map_or(2, usize::from);
            tch::set_num_threads(available.min(4) as i32);
        });
        let mut model =
            CModule::load_on_device(model_path, Device::Cpu).map_err(inference_error)?;
        model.set_eval();
        Ok(Self { model })
    }

    /// Write stereo, 32-bit float WAVs without replacing existing files.
    /// Memory is bounded to a few 8-second windows plus model inference memory.
    /// Cancellation is checked between windows and during decoding/writing.
    pub fn separate(
        &self,
        input: impl AsRef<Path>,
        vocals_path: impl AsRef<Path>,
        instrumental_path: impl AsRef<Path>,
        cancel: CancellationToken,
        on_progress: impl Fn(StemProgress),
    ) -> Result<StemReceipt> {
        check_cancel(&cancel)?;
        let vocals_path = vocals_path.as_ref();
        let instrumental_path = instrumental_path.as_ref();
        if vocals_path == instrumental_path {
            return Err(MediaError::InvalidInput(
                "Cada pista necesita un destino diferente.".into(),
            ));
        }
        if vocals_path.exists() || instrumental_path.exists() {
            return Err(MediaError::DestinationExists);
        }
        let decoded = decode_file(input)?;
        let errors = decoded.error_handle();
        let total_frames = decoded
            .total_duration()
            .map(|d| (d.as_secs_f64() * RATE as f64).round() as u64);
        // Rodio performs channel mapping and rate conversion in-process. The
        // separation output is always stereo/44.1 kHz, including mono sources.
        let mut samples = UniformSourceIterator::new(decoded, 2, RATE);
        let mut vocals_temp = temporary_wav(vocals_path)?;
        let mut instrumental_temp = temporary_wav(instrumental_path)?;
        let spec = WavSpec {
            channels: 2,
            sample_rate: RATE,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        let mut vocals_writer =
            WavWriter::new(vocals_temp.as_file_mut(), spec).map_err(wav_error)?;
        let mut instrumental_writer =
            WavWriter::new(instrumental_temp.as_file_mut(), spec).map_err(wav_error)?;
        let mut window = Vec::with_capacity(BLOCK * 2);
        let mut accum = vec![0.0_f32; BLOCK * 2];
        let mut weights = vec![0.0_f32; BLOCK];
        let taper = overlap_window();
        let mut processed = 0_u64;
        let _no_grad = tch::no_grad_guard();
        on_progress(StemProgress {
            processed_frames: 0,
            total_frames,
        });
        loop {
            check_cancel(&cancel)?;
            let missing = BLOCK * 2 - window.len();
            for (index, sample) in samples.by_ref().take(missing).enumerate() {
                if index % 8192 == 0 {
                    check_cancel(&cancel)?;
                }
                if !sample.is_finite() {
                    return Err(MediaError::Processing(
                        "El audio contiene muestras no válidas.".into(),
                    ));
                }
                window.push(sample);
            }
            if let Some(error) = errors.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                return Err(MediaError::Processing(error));
            }
            let frames = window.len() / 2;
            if frames == 0 {
                break;
            }
            let end = frames < BLOCK;
            let vocals = self.predict(&window)?;
            check_cancel(&cancel)?;
            for frame in 0..frames {
                weights[frame] += taper[frame];
                accum[frame * 2] += vocals[frame] * taper[frame];
                accum[frame * 2 + 1] += vocals[BLOCK + frame] * taper[frame];
            }
            let emit = if end { frames } else { HOP };
            for (frame, weight) in weights.iter().take(emit).enumerate() {
                if frame % 4096 == 0 {
                    check_cancel(&cancel)?;
                }
                for channel in 0..2 {
                    let index = frame * 2 + channel;
                    let vocal = accum[index] / weight;
                    vocals_writer.write_sample(vocal).map_err(wav_error)?;
                    instrumental_writer
                        .write_sample(window[index] - vocal)
                        .map_err(wav_error)?;
                }
            }
            processed += emit as u64;
            on_progress(StemProgress {
                processed_frames: processed,
                total_frames,
            });
            if end {
                break;
            }
            window.drain(..emit * 2);
            accum.copy_within(emit * 2.., 0);
            accum[(BLOCK - emit) * 2..].fill(0.0);
            weights.copy_within(emit.., 0);
            weights[BLOCK - emit..].fill(0.0);
        }
        if processed == 0 {
            return Err(MediaError::Processing(
                "El archivo no contiene audio.".into(),
            ));
        }
        check_cancel(&cancel)?;
        vocals_writer.finalize().map_err(wav_error)?;
        instrumental_writer.finalize().map_err(wav_error)?;
        vocals_temp.as_file().sync_all()?;
        instrumental_temp.as_file().sync_all()?;
        persist(vocals_temp, vocals_path)?;
        if let Err(error) = persist(instrumental_temp, instrumental_path) {
            // The first output was created by this operation. Roll it back if
            // publishing the second output fails; pre-existing files survive.
            let _ = fs::remove_file(vocals_path);
            return Err(error);
        }
        on_progress(StemProgress {
            processed_frames: processed,
            total_frames: Some(processed),
        });
        Ok(StemReceipt {
            vocals: vocals_path.to_owned(),
            instrumental: instrumental_path.to_owned(),
            frames: processed,
            sample_rate: RATE,
        })
    }

    fn predict(&self, interleaved: &[f32]) -> Result<Vec<f32>> {
        let frames = interleaved.len() / 2;
        let mut planar = vec![0.0_f32; BLOCK * 2];
        for frame in 0..frames {
            planar[frame] = interleaved[frame * 2];
            planar[BLOCK + frame] = interleaved[frame * 2 + 1];
        }
        let input = Tensor::f_from_slice(&planar)
            .and_then(|t| t.f_reshape([1, 2, BLOCK as i64]))
            .map_err(inference_error)?;
        let output = self.model.forward_ts(&[input]).map_err(inference_error)?;
        if output.size() != [1, 2, BLOCK as i64] {
            return Err(MediaError::Processing(
                "El modelo no es compatible: se requiere BS-RoFormer estéreo de 8 segundos.".into(),
            ));
        }
        let output = output.f_contiguous().map_err(inference_error)?;
        output
            .f_copy_data(&mut planar, BLOCK * 2)
            .map_err(inference_error)?;
        if planar.iter().any(|value| !value.is_finite()) {
            return Err(MediaError::Processing(
                "El modelo produjo muestras no válidas.".into(),
            ));
        }
        Ok(planar)
    }
}

fn overlap_window() -> Vec<f32> {
    // Positive endpoints avoid dividing by zero at the first/last audio frame.
    (0..BLOCK)
        .map(|i| {
            let phase = (i as f32 + 0.5) / BLOCK as f32;
            (std::f32::consts::PI * phase).sin().powi(2).max(0.001)
        })
        .collect()
}

fn temporary_wav(destination: &Path) -> Result<tempfile::NamedTempFile> {
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    Ok(tempfile::Builder::new()
        .prefix(".yt-stem-")
        .suffix(".part")
        .tempfile_in(parent)?)
}

fn persist(file: tempfile::NamedTempFile, destination: &Path) -> Result<()> {
    file.persist_noclobber(destination)
        .map(|_| ())
        .map_err(|error| {
            if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                MediaError::DestinationExists
            } else {
                MediaError::Io(error.error)
            }
        })
}

fn check_cancel(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(MediaError::Cancelled)
    } else {
        Ok(())
    }
}

fn inference_error(error: tch::TchError) -> MediaError {
    MediaError::Processing(error.to_string())
}
fn wav_error(error: hound::Error) -> MediaError {
    MediaError::Processing(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_remain_positive_and_complementary() {
        let window = overlap_window();
        assert!(window.iter().all(|w| w.is_finite() && *w > 0.0));
        for frame in HOP..HOP * 2 {
            let sum: f32 = (0..4).map(|i| window[(frame + i * HOP) % BLOCK]).sum();
            assert!((sum - 2.0).abs() < 0.004);
        }
    }

    #[test]
    fn publishing_never_replaces_existing_audio() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vocals.wav");
        fs::write(&path, b"original").unwrap();
        let temp = temporary_wav(&path).unwrap();
        assert!(matches!(
            persist(temp, &path),
            Err(MediaError::DestinationExists)
        ));
        assert_eq!(fs::read(path).unwrap(), b"original");
    }
}
