use super::BLOCK;
use crate::{MediaError, Result};
use std::{
    path::{Path, PathBuf},
    sync::Once,
};
use tch::{CModule, Device, Tensor};

pub const STEM_MODEL_FILENAME: &str = "bs-roformer-v1.pt";
pub const STEM_MODEL_LABEL: &str = "BS-RoFormer viperx SDR 12.9755";
static THREADS: Once = Once::new();

pub(super) struct Backend {
    model_path: PathBuf,
    model: Option<CModule>,
    device: Device,
    fallback_reason: Option<String>,
}

impl Backend {
    pub(super) fn load(model_path: &Path) -> Result<Self> {
        THREADS.call_once(|| {
            let available = std::thread::available_parallelism().map_or(2, usize::from);
            tch::set_num_threads(available.min(4) as i32);
        });
        let model_path = model_path.to_owned();
        let preferred = preferred_device(tch::Cuda::is_available(), tch::utils::has_mps());
        let (model, device, fallback_reason) = match load_model(&model_path, preferred) {
            Ok(model) => (model, preferred, None),
            Err(error) if preferred != Device::Cpu => (
                load_model(&model_path, Device::Cpu)?,
                Device::Cpu,
                Some(fallback_message(preferred, &error)),
            ),
            Err(error) => return Err(error),
        };
        Ok(Self {
            model_path,
            model: Some(model),
            device,
            fallback_reason,
        })
    }

    pub(super) fn device_name(&self) -> &'static str {
        device_label(self.device)
    }

    pub(super) fn fallback_reason(&self) -> Option<String> {
        self.fallback_reason.clone()
    }

    pub(super) fn predict(&mut self, planar: &[f32], vocals: &mut [f32]) -> Result<()> {
        let _no_grad = tch::no_grad_guard();
        match predict_on(self, planar, vocals) {
            Ok(()) => Ok(()),
            Err(error) if self.device != Device::Cpu => {
                self.fallback_reason = Some(fallback_message(self.device, &error));
                // Release device allocations before loading the fallback model.
                self.model.take();
                self.device = Device::Cpu;
                self.model = Some(load_model(&self.model_path, Device::Cpu)?);
                predict_on(self, planar, vocals)
            }
            Err(error) => Err(error),
        }
    }
}

fn predict_on(backend: &Backend, planar: &[f32], samples: &mut [f32]) -> Result<()> {
    let input = Tensor::f_from_slice(planar)
        .and_then(|t| t.f_reshape([1, 2, BLOCK as i64]))
        .and_then(|t| t.f_to_device(backend.device))
        .map_err(inference_error)?;
    let model = backend
        .model
        .as_ref()
        .ok_or_else(|| MediaError::Processing("No se pudo cargar el modelo de respaldo".into()))?;
    let output = model.forward_ts(&[input]).map_err(inference_error)?;
    if output.size() != [1, 2, BLOCK as i64] {
        return Err(MediaError::Processing(
            "El modelo no es compatible: se requiere BS-RoFormer estéreo de 8 segundos.".into(),
        ));
    }
    let output = output
        .f_to_device(Device::Cpu)
        .and_then(|t| t.f_contiguous())
        .map_err(inference_error)?;
    output
        .f_copy_data(samples, BLOCK * 2)
        .map_err(inference_error)?;
    if samples.iter().any(|value| !value.is_finite()) {
        return Err(MediaError::Processing(
            "El modelo produjo muestras no válidas.".into(),
        ));
    }
    Ok(())
}

fn load_model(path: &Path, device: Device) -> Result<CModule> {
    let mut model = CModule::load_on_device(path, device).map_err(inference_error)?;
    model.f_set_eval().map_err(inference_error)?;
    Ok(model)
}

fn preferred_device(cuda: bool, mps: bool) -> Device {
    if cuda {
        Device::Cuda(0)
    } else if mps {
        Device::Mps
    } else {
        Device::Cpu
    }
}

fn device_label(device: Device) -> &'static str {
    match device {
        Device::Cuda(_) => "CUDA",
        Device::Mps => "Apple Metal",
        _ => "CPU",
    }
}

fn fallback_message(device: Device, error: &MediaError) -> String {
    // Native exception backtraces can include build-machine paths and hundreds
    // of frames; the first line contains the actionable reason.
    let message = error.to_string();
    let reason = message.lines().next().unwrap_or("Error de inferencia");
    format!(
        "{}: {}",
        device_label(device),
        reason.chars().take(512).collect::<String>()
    )
}

fn inference_error(error: tch::TchError) -> MediaError {
    MediaError::Processing(error.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::stems::prepare_window;

    #[test]
    fn failed_accelerator_reloads_cpu_and_retries_original_samples() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.pt");
        let input = Tensor::zeros([1, 2, BLOCK as i64], (tch::Kind::Float, Device::Cpu));
        let model = CModule::create_by_tracing("Fixture", "forward", &[input], &mut |inputs| {
            vec![&inputs[0] * 2.0]
        })
        .unwrap();
        model.save(&path).unwrap();
        let mut backend = Backend {
            model_path: path,
            model: None,
            device: Device::Cuda(0),
            fallback_reason: None,
        };
        let mut planar = vec![0.0; BLOCK * 2];
        let mut output = vec![0.0; BLOCK * 2];
        prepare_window(&[0.25, -0.5, 0.75, 0.125], &mut planar);
        backend.predict(&planar, &mut output).unwrap();
        assert_eq!(&output[..2], &[0.5, 1.5]);
        assert_eq!(&output[BLOCK..BLOCK + 2], &[-1.0, 0.25]);
        assert_eq!(backend.device_name(), "CPU");
        assert!(backend.fallback_reason().unwrap().starts_with("CUDA:"));
        prepare_window(&[0.5, 0.75], &mut planar);
        backend.predict(&planar, &mut output).unwrap();
        assert_eq!(&output[..2], &[1.0, 0.0]);
        assert_eq!(&output[BLOCK..BLOCK + 2], &[1.5, 0.0]);
    }

    #[test]
    fn detection_preserves_cpu_and_diagnostics_drop_native_backtrace() {
        assert_eq!(preferred_device(false, false), Device::Cpu);
        assert_eq!(preferred_device(false, true), Device::Mps);
        assert_eq!(preferred_device(true, true), Device::Cuda(0));
        let error = MediaError::Processing(
            "Unsupported operation\n/private/developer/model.py\nframe #0".into(),
        );
        let message = fallback_message(Device::Mps, &error);
        assert!(message.contains("Unsupported operation"));
        assert!(!message.contains("developer"));
        assert!(!message.contains("frame"));
    }
}
