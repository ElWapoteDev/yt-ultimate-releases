/// How an analysis fails.
#[derive(Debug, thiserror::Error)]
pub enum MirError {
    /// The samples cannot be analyzed (rate, channels, length or values).
    #[error("Audio: {0}")]
    InvalidAudio(String),
    /// The beat model file is missing, altered or has an unexpected layout.
    #[error("Model: {0}")]
    Model(String),
    /// ONNX Runtime could not load, compile or run the model.
    #[error("ONNX Runtime: {0}")]
    Runtime(String),
    /// The caller cancelled the analysis.
    #[error("The analysis was cancelled")]
    Cancelled,
    /// The result broke a [`yt_core::TrackAnalysis`] bound.
    #[error(transparent)]
    Core(#[from] yt_core::CoreError),
}

/// Result of every fallible operation in this crate.
pub type Result<T> = std::result::Result<T, MirError>;

pub(crate) fn runtime(error: impl std::fmt::Display) -> MirError {
    MirError::Runtime(error.to_string())
}
