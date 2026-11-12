//! The versioned job protocol between the application and `ytu-worker`.
//!
//! Every message is one frame: its JSON length as a little-endian `u32`, then
//! the JSON. The worker first sends [`WorkerMessage::Ready`]; after that each
//! [`Request`] names a job, and the worker answers every job it started with
//! exactly one [`Reply`]. Both sides treat what they read as untrusted input:
//! frames are bounded, versions checked and results validated.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::path::PathBuf;
use yt_core::{KeyEstimate, MediaOrigin, TrackAnalysis};
use yt_mir::Device;
use yt_mix::Fusion;

/// Version of this protocol. Raise it whenever a message changes shape; both
/// sides refuse messages of another version.
pub const PROTOCOL_VERSION: u16 = 4;
/// Largest frame accepted, in bytes (an analysis with 20,000 beats takes about
/// 0.5 MB).
pub const MAX_FRAME_BYTES: usize = 4 << 20;
/// Length prefix of a frame, in bytes.
pub const FRAME_HEADER_BYTES: usize = 4;
/// Longest failure detail sent in a reply, in characters.
pub const MAX_DETAIL_CHARS: usize = 300;

/// Identifies one job of one client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(pub u64);

/// A message from the application to the worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub protocol_version: u16,
    pub job: JobId,
    pub action: Action,
}

/// What a request asks for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Measure beats, bars, meter, tempo, key and loudness of the audio file
    /// at `path` (read only), whose rights follow `origin`.
    AnalyzeTrack { origin: MediaOrigin, path: PathBuf },
    /// Measure how loud the vocals of the audio file at `path` are every
    /// half second (`TrackAnalysis::vocal_db`). The separated audio never
    /// leaves the worker's memory, so this is analysis for `origin`'s rights.
    MeasureVocals { origin: MediaOrigin, path: PathBuf },
    /// Separate the vocals of source frames `first_frame..end_frame` of the
    /// audio file at `path` for a mix that plays them apart, and write them
    /// to the new file `output` as little-endian `f32`, interleaved at the
    /// audio's rate and channels (mono or stereo). Needs `origin`'s
    /// `separate` right; the caller removes the file after the mix.
    SeparateVocals {
        origin: MediaOrigin,
        path: PathBuf,
        first_frame: u64,
        end_frame: u64,
        output: PathBuf,
    },
    /// Render an AI fusion and write its clip to the request's output.
    RenderFusion(FusionRequest),
    /// Stop the request's job if it is running. The job still gets its one
    /// reply: [`Outcome::Cancelled`], or its result if it had finished.
    Cancel,
}

/// An AI fusion of `outgoing` into `incoming` (played at `speed`), laid out
/// by `fusion`, rendered by a model prompted with `tempo_bpm` and `key`. Its
/// clip goes to the new file `output` as little-endian `f32`, stereo at
/// `fusion::FUSION_RATE`. Needs both origins' `generate` right; the caller
/// removes the file after the mix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FusionRequest {
    pub outgoing: FusionSide,
    pub incoming: FusionSide,
    pub speed: f32,
    pub fusion: Fusion,
    pub tempo_bpm: f32,
    pub key: Option<KeyEstimate>,
    pub seed: u32,
    pub output: PathBuf,
}

/// One song of a fusion: its audio file, whose rights follow `origin`, and
/// the source frame where the fusion starts in it (the outgoing song's exit)
/// or the incoming deck starts after it (the incoming song's entry).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FusionSide {
    pub origin: MediaOrigin,
    pub path: PathBuf,
    pub frame: u64,
}

/// A message from the worker to the application.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerMessage {
    /// Sent once at start: the worker speaks `protocol_version` and its
    /// analyses carry `analyzer_version`.
    Ready {
        protocol_version: u16,
        analyzer_version: u16,
    },
    /// The one answer to a job.
    Reply(Reply),
}

/// The answer to one job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub protocol_version: u16,
    pub job: JobId,
    pub outcome: Outcome,
}

/// How a job ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The analysis, made on `device`.
    Analysis {
        analysis: Box<TrackAnalysis>,
        device: Device,
    },
    /// The vocal levels, measured on `device`.
    Vocals { vocal_db: Vec<f32>, device: Device },
    /// The vocals were written to the job's output: `frames` frames of
    /// `channels` channels at `sample_rate`.
    Separated {
        frames: u64,
        channels: u16,
        sample_rate: u32,
    },
    /// The fusion's clip was written to the job's output: `frames` stereo
    /// frames at `fusion::FUSION_RATE`.
    Fused { frames: u64 },
    /// Stopped by a [`Action::Cancel`] or because the application went away.
    Cancelled,
    /// Not run now because of the device's power or temperature; the caller
    /// may ask again later.
    Deferred { reason: DeferReason },
    /// Could not be done; `detail` is a short explanation without paths.
    Failed { failure: Failure, detail: String },
}

/// Why the governor holds work back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferReason {
    /// Low Power Mode, battery saver or a nearly empty battery.
    LowPower,
    /// The device is too hot.
    Thermal,
}

/// Why a job failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    /// The request was malformed or of another protocol version.
    Protocol,
    /// The origin's [`yt_core::SourceRights`] forbid the job.
    Rights,
    /// The audio could not be read or is outside the analysis limits.
    Audio,
    /// This device cannot run the job: no runtime, model or usable GPU. It
    /// stays so until the application is restarted.
    Unsupported,
    /// The model failed while running.
    Runtime,
}

/// A frame that cannot be accepted.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("a frame of {0} bytes exceeds the limit")]
    TooLarge(usize),
    #[error("malformed message: {0}")]
    Malformed(#[from] serde_json::Error),
    #[error("protocol version {0} is not {PROTOCOL_VERSION}")]
    Version(u16),
}

/// The bytes of one frame carrying `message`.
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, ProtocolError> {
    let json = serde_json::to_vec(message)?;
    if json.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::TooLarge(json.len()));
    }
    let length = u32::try_from(json.len()).map_err(|_| ProtocolError::TooLarge(json.len()))?;
    let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + json.len());
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(&json);
    Ok(frame)
}

/// The body length a frame header announces, refused beyond
/// [`MAX_FRAME_BYTES`].
pub fn frame_length(header: [u8; FRAME_HEADER_BYTES]) -> Result<usize, ProtocolError> {
    let length = u32::from_le_bytes(header) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(ProtocolError::TooLarge(length));
    }
    Ok(length)
}

/// Parse one frame body.
pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, ProtocolError> {
    Ok(serde_json::from_slice(body)?)
}

/// The job a request body names, read even when the rest of it cannot be
/// parsed, so the worker can still answer it.
pub fn request_job(body: &[u8]) -> Option<JobId> {
    #[derive(Deserialize)]
    struct Header {
        job: JobId,
    }
    serde_json::from_slice::<Header>(body)
        .ok()
        .map(|header| header.job)
}

/// `detail` cut to [`MAX_DETAIL_CHARS`].
pub fn bounded_detail(detail: &str) -> String {
    detail.chars().take(MAX_DETAIL_CHARS).collect()
}

impl Request {
    /// A request of this protocol version.
    pub fn new(job: JobId, action: Action) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            job,
            action,
        }
    }

    /// `Err(Version)` unless the request speaks [`PROTOCOL_VERSION`].
    pub fn check_version(&self) -> Result<(), ProtocolError> {
        check_version(self.protocol_version)
    }
}

impl Reply {
    /// A reply of this protocol version.
    pub fn new(job: JobId, outcome: Outcome) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            job,
            outcome,
        }
    }

    /// A failure reply with `detail` bounded to [`MAX_DETAIL_CHARS`].
    pub fn failed(job: JobId, failure: Failure, detail: &str) -> Self {
        Self::new(
            job,
            Outcome::Failed {
                failure,
                detail: bounded_detail(detail),
            },
        )
    }
}

/// `Err(Version)` unless `version` is [`PROTOCOL_VERSION`].
pub fn check_version(version: u16) -> Result<(), ProtocolError> {
    if version == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ProtocolError::Version(version))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_with_a_little_endian_length() {
        let request = Request::new(
            JobId(7),
            Action::AnalyzeTrack {
                origin: MediaOrigin::Library,
                path: PathBuf::from("/music/song.flac"),
            },
        );
        let frame = encode(&request).unwrap();
        let header: [u8; FRAME_HEADER_BYTES] = frame[..FRAME_HEADER_BYTES].try_into().unwrap();
        let length = frame_length(header).unwrap();
        assert_eq!(length, frame.len() - FRAME_HEADER_BYTES);
        let body = &frame[FRAME_HEADER_BYTES..];
        assert_eq!(decode::<Request>(body).unwrap(), request);
        assert_eq!(request_job(body), Some(JobId(7)));
    }

    #[test]
    fn oversized_frames_and_other_versions_are_refused() {
        let header = u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_le_bytes();
        assert!(matches!(
            frame_length(header),
            Err(ProtocolError::TooLarge(_))
        ));
        let mut request = Request::new(JobId(1), Action::Cancel);
        assert!(request.check_version().is_ok());
        request.protocol_version = PROTOCOL_VERSION + 1;
        assert!(matches!(
            request.check_version(),
            Err(ProtocolError::Version(_))
        ));
    }

    #[test]
    fn a_job_is_found_even_in_a_request_that_does_not_parse() {
        let body = br#"{"protocol_version":9,"job":42,"action":{"from_the_future":{}}}"#;
        assert!(decode::<Request>(body).is_err());
        assert_eq!(request_job(body), Some(JobId(42)));
        assert_eq!(request_job(b"not json"), None);
    }

    #[test]
    fn failure_details_are_bounded() {
        let reply = Reply::failed(JobId(3), Failure::Audio, &"x".repeat(1_000));
        match reply.outcome {
            Outcome::Failed { detail, .. } => assert_eq!(detail.len(), MAX_DETAIL_CHARS),
            other => panic!("unexpected outcome {other:?}"),
        }
    }
}
