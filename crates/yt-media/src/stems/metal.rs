use super::BLOCK;
use crate::{MediaError, Result};
use std::path::Path;
use ytu_candle_roformer_metal::{BsRoFormer, WINDOW_FRAMES};

pub const STEM_MODEL_FILENAME: &str = "bs-roformer-viperx-f16.safetensors";
pub const STEM_MODEL_LABEL: &str = "BS-RoFormer viperx SDR 12.9755";
const _: () = assert!(BLOCK == WINDOW_FRAMES);

pub(super) struct Backend(BsRoFormer);

impl Backend {
    pub(super) fn load(model_path: &Path) -> Result<Self> {
        Ok(Self(BsRoFormer::load(model_path).map_err(metal_error)?))
    }

    pub(super) fn device_name(&self) -> &'static str {
        "Apple Metal"
    }

    pub(super) fn fallback_reason(&self) -> Option<String> {
        None
    }

    pub(super) fn predict(&mut self, planar: &[f32], vocals: &mut [f32]) -> Result<()> {
        self.0.predict(planar, vocals).map_err(metal_error)?;
        if vocals.iter().any(|sample| !sample.is_finite()) {
            return Err(MediaError::Processing(
                "El modelo produjo muestras no válidas.".into(),
            ));
        }
        Ok(())
    }
}

fn metal_error(error: impl std::fmt::Display) -> MediaError {
    MediaError::Processing(format!("Apple Metal: {error}"))
}
