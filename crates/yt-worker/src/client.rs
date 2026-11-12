//! Runs `ytu-worker` for the application: starts it with the first job,
//! sends one job at a time, validates every reply as untrusted input, and
//! restarts it with a growing wait after a crash. The worker exits by itself
//! after [`WorkerConfig::idle_exit`] without jobs, freeing its memory.

use crate::{
    fusion::{Generator, FUSION_RATE},
    protocol::{
        self, decode, encode, frame_length, Action, DeferReason, Failure, FusionRequest, JobId,
        Outcome, Request, WorkerMessage, FRAME_HEADER_BYTES, PROTOCOL_VERSION,
    },
    EXIT_CONFIGURATION,
};
use std::{
    collections::VecDeque,
    path::PathBuf,
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{mpsc, Mutex},
};
use tokio_util::sync::CancellationToken;
use yt_core::{MediaOrigin, TrackAnalysis};
use yt_mir::{Device, ANALYZER_VERSION};

/// Time without jobs after which the worker exits (`docs/AI_DJ.md` §5.8).
pub const IDLE_EXIT: Duration = Duration::from_secs(180);
/// Longest wait for a starting worker to announce itself.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest wait for a job's reply after asking the worker to cancel it; a
/// worker that does not answer by then is stopped.
const CANCEL_GRACE: Duration = Duration::from_secs(5);
/// A worker this close to its idle exit (at most half of it) is replaced
/// instead of reused, so a job never races the exit.
const IDLE_MARGIN: Duration = Duration::from_secs(10);
/// Wait before restarting after the first crash; it doubles with each crash.
const RESTART_DELAY: Duration = Duration::from_secs(2);
/// Longest wait between restarts.
const MAX_RESTART_DELAY: Duration = Duration::from_secs(60);
/// Crashes tolerated within [`RESTART_WINDOW`]; after them the worker stays
/// down until the oldest leaves the window.
const MAX_CRASHES: usize = 5;
const RESTART_WINDOW: Duration = Duration::from_secs(3_600);
/// Replies buffered between the reader task and the job waiting for them.
const EVENT_BUFFER: usize = 4;

/// Where the worker and what it loads are.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// The `ytu-worker` executable.
    pub executable: PathBuf,
    /// The ONNX Runtime library the worker loads.
    pub runtime_library: PathBuf,
    /// The beat model ([`yt_mir::BEAT_MODEL_FILENAME`]).
    pub beat_model: PathBuf,
    /// The vocal model ([`yt_mir::VOCAL_MODEL_FILENAME`]); without it,
    /// [`WorkerClient::measure_vocals`] fails as unsupported.
    pub vocal_model: Option<PathBuf>,
    /// The generative model of AI fusions; without it,
    /// [`WorkerClient::render_fusion`] fails as unsupported.
    pub fusion: Option<Generator>,
    /// Folder where CoreML keeps compiled models.
    pub model_cache: PathBuf,
    /// Time without jobs after which the worker exits; [`IDLE_EXIT`] in the
    /// application.
    pub idle_exit: Duration,
}

/// A validated analysis and where it was computed.
#[derive(Debug, Clone, PartialEq)]
pub struct Analyzed {
    pub analysis: TrackAnalysis,
    pub device: Device,
}

/// Vocal levels of a song (`TrackAnalysis::vocal_db`) and where they were
/// measured.
#[derive(Debug, Clone, PartialEq)]
pub struct Vocals {
    pub vocal_db: Vec<f32>,
    pub device: Device,
}

/// Vocals separated into a file for one mix.
#[derive(Debug, Clone, PartialEq)]
pub struct Separated {
    pub frames: u64,
    pub channels: u16,
    pub sample_rate: u32,
}

/// Why a job brought no result.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ClientError {
    /// The worker cannot run on this device (missing files, another version,
    /// no usable runtime). It stays so for the life of the client.
    #[error("the DJ worker is unavailable: {0}")]
    Unavailable(String),
    /// The worker crashed recently and restarts after this wait.
    #[error("the DJ worker restarts in {0:?}")]
    Restarting(Duration),
    #[error("cancelled")]
    Cancelled,
    /// The power governor holds work back; ask again later.
    #[error("held back by the power governor ({0:?})")]
    Deferred(DeferReason),
    #[error("the job failed ({failure:?}): {detail}")]
    Failed { failure: Failure, detail: String },
    #[error("the DJ worker stopped unexpectedly")]
    Crashed,
    /// The worker answered something invalid; it was stopped.
    #[error("the DJ worker sent an invalid reply: {0}")]
    InvalidReply(String),
}

/// Starts and talks to one worker process. Jobs run one at a time, in the
/// order they acquire the client.
pub struct WorkerClient {
    config: WorkerConfig,
    slot: Mutex<Slot>,
    next_job: AtomicU64,
}

#[derive(Default)]
struct Slot {
    process: Option<Process>,
    restarts: Restarts,
    unavailable: Option<String>,
    /// Why each optional model cannot run on this device, while analysis
    /// still can (indexed by [`OptionalModel`]).
    optional_unavailable: [Option<String>; 2],
}

/// A model only some jobs need; the worker runs the others without it.
#[derive(Debug, Clone, Copy)]
enum OptionalModel {
    Vocals = 0,
    Fusions = 1,
}

impl OptionalModel {
    fn of(action: &Action) -> Option<Self> {
        match action {
            Action::MeasureVocals { .. } | Action::SeparateVocals { .. } => Some(Self::Vocals),
            Action::RenderFusion(_) => Some(Self::Fusions),
            Action::AnalyzeTrack { .. } | Action::Cancel => None,
        }
    }
}

struct Process {
    // Held so dropping the process kills the child (`kill_on_drop`).
    child: Child,
    stdin: ChildStdin,
    events: mpsc::Receiver<WorkerMessage>,
    last_used: Instant,
}

enum Start {
    Unavailable(String),
    Crashed,
}

enum Exchange {
    Done(Outcome),
    Closed,
    Abandoned,
    Invalid(String),
}

impl WorkerClient {
    /// A client that starts the worker described by `config` when needed.
    pub fn new(config: WorkerConfig) -> Self {
        Self {
            config,
            slot: Mutex::new(Slot::default()),
            next_job: AtomicU64::new(1),
        }
    }

    /// Whether this client can measure vocals: it was given a vocal model.
    pub fn measures_vocals(&self) -> bool {
        self.config.vocal_model.is_some()
    }

    /// Whether this client can render fusions: it was given their model.
    pub fn renders_fusions(&self) -> bool {
        self.config.fusion.is_some()
    }

    /// Render the AI fusion `request` describes; the clip's length is
    /// checked against its fusion. The caller reads the output file and
    /// removes it. Waits for earlier jobs; `cancel` stops the wait or the job.
    pub async fn render_fusion(
        &self,
        request: FusionRequest,
        cancel: &CancellationToken,
    ) -> Result<u64, ClientError> {
        let expected = request.fusion.clip_frames(FUSION_RATE);
        match self.run(Action::RenderFusion(request), cancel).await? {
            Outcome::Fused { frames } if frames == expected => Ok(frames),
            Outcome::Fused { .. } => Err(ClientError::InvalidReply(
                "the fusion has another length".into(),
            )),
            other => Err(unanswered(other)),
        }
    }

    /// Analyze the audio file at `path`, whose rights follow `origin`. The
    /// result is validated before it is returned. Waits for earlier jobs;
    /// `cancel` stops the wait or the job.
    pub async fn analyze(
        &self,
        origin: MediaOrigin,
        path: PathBuf,
        cancel: &CancellationToken,
    ) -> Result<Analyzed, ClientError> {
        match self
            .run(Action::AnalyzeTrack { origin, path }, cancel)
            .await?
        {
            Outcome::Analysis { analysis, device } => {
                if analysis.analyzer_version != ANALYZER_VERSION {
                    return Err(ClientError::InvalidReply(
                        "the analysis is of another analyzer version".into(),
                    ));
                }
                analysis
                    .validate()
                    .map_err(|error| ClientError::InvalidReply(error.to_string()))?;
                Ok(Analyzed {
                    analysis: *analysis,
                    device,
                })
            }
            other => Err(unanswered(other)),
        }
    }

    /// Separate the vocals of source frames `frames` of the audio file at
    /// `path`, whose rights follow `origin`, into the new file `output`
    /// (little-endian `f32`, interleaved). The caller reads the file, checks
    /// its length against the answer, and removes it. Waits for earlier jobs;
    /// `cancel` stops the wait or the job.
    pub async fn separate_vocals(
        &self,
        origin: MediaOrigin,
        path: PathBuf,
        frames: std::ops::Range<u64>,
        output: PathBuf,
        cancel: &CancellationToken,
    ) -> Result<Separated, ClientError> {
        let action = Action::SeparateVocals {
            origin,
            path,
            first_frame: frames.start,
            end_frame: frames.end,
            output,
        };
        match self.run(action, cancel).await? {
            Outcome::Separated {
                frames: written,
                channels,
                sample_rate,
            } if written == frames.end - frames.start => Ok(Separated {
                frames: written,
                channels,
                sample_rate,
            }),
            Outcome::Separated { .. } => Err(ClientError::InvalidReply(
                "the separated vocals have another length".into(),
            )),
            other => Err(unanswered(other)),
        }
    }

    /// Measure how loud the vocals of the audio file at `path` are every
    /// half second, whose rights follow `origin`. Every level is checked
    /// against `TrackAnalysis::LEVEL_DB` and the count against the longest
    /// track; the caller checks it against the song's own duration
    /// (`TrackAnalysis::validate`). Waits for earlier jobs; `cancel` stops the
    /// wait or the job.
    pub async fn measure_vocals(
        &self,
        origin: MediaOrigin,
        path: PathBuf,
        cancel: &CancellationToken,
    ) -> Result<Vocals, ClientError> {
        match self
            .run(Action::MeasureVocals { origin, path }, cancel)
            .await?
        {
            Outcome::Vocals { vocal_db, device } => {
                let steps = (f64::from(TrackAnalysis::MAX_DURATION_SECONDS)
                    / f64::from(TrackAnalysis::LOUDNESS_STEP_SECONDS))
                .ceil() as usize
                    + 1;
                let valid = vocal_db.len() <= steps
                    && vocal_db
                        .iter()
                        .all(|db| db.is_finite() && TrackAnalysis::LEVEL_DB.contains(db));
                if !valid {
                    return Err(ClientError::InvalidReply(
                        "the vocal levels are out of range".into(),
                    ));
                }
                Ok(Vocals { vocal_db, device })
            }
            other => Err(unanswered(other)),
        }
    }

    async fn run(
        &self,
        action: Action,
        cancel: &CancellationToken,
    ) -> Result<Outcome, ClientError> {
        let mut slot = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ClientError::Cancelled),
            slot = self.slot.lock() => slot,
        };
        let optional = OptionalModel::of(&action);
        let unavailable = slot
            .unavailable
            .as_ref()
            .or(optional.and_then(|model| slot.optional_unavailable[model as usize].as_ref()));
        if let Some(reason) = unavailable {
            return Err(ClientError::Unavailable(reason.clone()));
        }
        let job = JobId(self.next_job.fetch_add(1, Ordering::Relaxed));
        let request = encode(&Request::new(job, action)).map_err(|error| ClientError::Failed {
            failure: Failure::Protocol,
            detail: error.to_string(),
        })?;
        // A worker that exits cleanly (idle) before reading the job gets one
        // replacement; any other exit counts as a crash.
        for attempt in 0..2 {
            let margin = IDLE_MARGIN.min(self.config.idle_exit / 2);
            if slot.process.as_ref().is_some_and(|process| {
                process.last_used.elapsed() + margin >= self.config.idle_exit
            }) {
                slot.process = None;
            }
            if slot.process.is_none() {
                if let Some(wait) = slot.restarts.wait(Instant::now()) {
                    return Err(ClientError::Restarting(wait));
                }
                match self.start().await {
                    Ok(process) => slot.process = Some(process),
                    Err(Start::Unavailable(reason)) => {
                        slot.unavailable = Some(reason.clone());
                        return Err(ClientError::Unavailable(reason));
                    }
                    Err(Start::Crashed) => {
                        slot.restarts.crashed(Instant::now());
                        return Err(ClientError::Crashed);
                    }
                }
            }
            let Some(process) = slot.process.as_mut() else {
                return Err(ClientError::Crashed);
            };
            match exchange(process, job, &request, cancel).await {
                Exchange::Done(Outcome::Failed {
                    failure: Failure::Unsupported,
                    detail,
                }) => {
                    let reason = protocol::bounded_detail(&detail);
                    if let Some(model) = optional {
                        process.last_used = Instant::now();
                        slot.optional_unavailable[model as usize] = Some(reason.clone());
                    } else {
                        slot.unavailable = Some(reason.clone());
                        slot.process = None;
                    }
                    return Err(ClientError::Unavailable(reason));
                }
                Exchange::Done(outcome) => {
                    process.last_used = Instant::now();
                    return Ok(outcome);
                }
                Exchange::Closed => {
                    let clean = match slot.process.take() {
                        Some(mut process) => exited_cleanly(&mut process.child).await,
                        None => false,
                    };
                    if clean && attempt == 0 {
                        continue;
                    }
                    slot.restarts.crashed(Instant::now());
                    return Err(ClientError::Crashed);
                }
                Exchange::Abandoned => {
                    slot.process = None;
                    return Err(ClientError::Cancelled);
                }
                Exchange::Invalid(reason) => {
                    slot.process = None;
                    slot.restarts.crashed(Instant::now());
                    return Err(ClientError::InvalidReply(reason));
                }
            }
        }
        Err(ClientError::Crashed)
    }

    async fn start(&self) -> Result<Process, Start> {
        let config = &self.config;
        let mut command = Command::new(&config.executable);
        command
            .arg("--runtime")
            .arg(&config.runtime_library)
            .arg("--beat-model")
            .arg(&config.beat_model)
            .arg("--model-cache")
            .arg(&config.model_cache)
            .arg("--idle-seconds")
            .arg(config.idle_exit.as_secs().max(1).to_string());
        if let Some(vocal_model) = &config.vocal_model {
            command.arg("--vocal-model").arg(vocal_model);
        }
        if let Some(fusion) = &config.fusion {
            command
                .arg("--fusion-program")
                .arg(fusion.program())
                .arg("--fusion-models")
                .arg(fusion.models());
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Its messages could name files the user played; nothing is kept.
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        let mut child = command
            .spawn()
            .map_err(|error| Start::Unavailable(format!("the worker cannot start: {error}")))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(Start::Crashed);
        };
        let (sender, mut events) = mpsc::channel(EVENT_BUFFER);
        tokio::spawn(read_events(stdout, sender));
        match tokio::time::timeout(START_TIMEOUT, events.recv()).await {
            Ok(Some(WorkerMessage::Ready {
                protocol_version,
                analyzer_version,
            })) => {
                if protocol_version != PROTOCOL_VERSION || analyzer_version != ANALYZER_VERSION {
                    return Err(Start::Unavailable(
                        "the worker belongs to another version of the application".into(),
                    ));
                }
                Ok(Process {
                    child,
                    stdin,
                    events,
                    last_used: Instant::now(),
                })
            }
            Ok(None) => {
                let status = tokio::time::timeout(START_TIMEOUT, child.wait()).await;
                match status {
                    Ok(Ok(status)) if status.code() == Some(EXIT_CONFIGURATION) => Err(
                        Start::Unavailable("the worker's runtime or model is missing".into()),
                    ),
                    _ => Err(Start::Crashed),
                }
            }
            Ok(Some(WorkerMessage::Reply(_))) | Err(_) => Err(Start::Crashed),
        }
    }
}

/// The error for an outcome that is not the result a job asked for.
fn unanswered(outcome: Outcome) -> ClientError {
    match outcome {
        Outcome::Cancelled => ClientError::Cancelled,
        Outcome::Deferred { reason } => ClientError::Deferred(reason),
        Outcome::Failed { failure, detail } => ClientError::Failed {
            failure,
            detail: protocol::bounded_detail(&detail),
        },
        Outcome::Analysis { .. }
        | Outcome::Vocals { .. }
        | Outcome::Separated { .. }
        | Outcome::Fused { .. } => {
            ClientError::InvalidReply("the worker answered another job's result".into())
        }
    }
}

/// Send `request` and wait for the reply to `job`, asking the worker to
/// cancel when `cancel` fires.
async fn exchange(
    process: &mut Process,
    job: JobId,
    request: &[u8],
    cancel: &CancellationToken,
) -> Exchange {
    if !send(&mut process.stdin, request).await {
        return Exchange::Closed;
    }
    let mut deadline = None;
    loop {
        let wait = deadline;
        let grace = async move {
            match wait {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            message = process.events.recv() => match message {
                Some(WorkerMessage::Reply(reply)) if reply.job == job => {
                    if reply.protocol_version != PROTOCOL_VERSION {
                        return Exchange::Invalid("a reply of another protocol version".into());
                    }
                    return Exchange::Done(reply.outcome);
                }
                Some(WorkerMessage::Reply(_)) => {}
                Some(WorkerMessage::Ready { .. }) => {
                    return Exchange::Invalid("an unexpected announcement".into());
                }
                None => return Exchange::Closed,
            },
            _ = cancel.cancelled(), if deadline.is_none() => {
                let Ok(frame) = encode(&Request::new(job, Action::Cancel)) else {
                    return Exchange::Abandoned;
                };
                if !send(&mut process.stdin, &frame).await {
                    return Exchange::Closed;
                }
                deadline = Some(tokio::time::Instant::now() + CANCEL_GRACE);
            }
            _ = grace => return Exchange::Abandoned,
        }
    }
}

async fn send(stdin: &mut ChildStdin, frame: &[u8]) -> bool {
    stdin.write_all(frame).await.is_ok() && stdin.flush().await.is_ok()
}

/// Forward the worker's messages until it closes its output or sends a frame
/// that cannot be accepted; either closes the channel.
async fn read_events(mut stdout: ChildStdout, events: mpsc::Sender<WorkerMessage>) {
    loop {
        let mut header = [0_u8; FRAME_HEADER_BYTES];
        if stdout.read_exact(&mut header).await.is_err() {
            return;
        }
        let Ok(length) = frame_length(header) else {
            return;
        };
        let mut body = vec![0_u8; length];
        if stdout.read_exact(&mut body).await.is_err() {
            return;
        }
        let Ok(message) = decode::<WorkerMessage>(&body) else {
            return;
        };
        if events.send(message).await.is_err() {
            return;
        }
    }
}

/// Whether `child` ended with success (an idle exit), waiting briefly for it.
async fn exited_cleanly(child: &mut Child) -> bool {
    matches!(
        tokio::time::timeout(START_TIMEOUT, child.wait()).await,
        Ok(Ok(status)) if status.success()
    )
}

/// Crash bookkeeping: the wait before a restart doubles from
/// [`RESTART_DELAY`] up to [`MAX_RESTART_DELAY`], and after [`MAX_CRASHES`]
/// crashes within [`RESTART_WINDOW`] the worker stays down until the oldest
/// leaves the window.
#[derive(Default)]
struct Restarts {
    crashes: VecDeque<Instant>,
}

impl Restarts {
    fn crashed(&mut self, now: Instant) {
        self.forget_old(now);
        self.crashes.push_back(now);
    }

    /// How long to wait before starting the worker again, if at all.
    fn wait(&mut self, now: Instant) -> Option<Duration> {
        self.forget_old(now);
        let (&oldest, &last) = (self.crashes.front()?, self.crashes.back()?);
        let ready = if self.crashes.len() >= MAX_CRASHES {
            oldest + RESTART_WINDOW
        } else {
            let doublings = u32::try_from(self.crashes.len() - 1).unwrap_or(u32::MAX);
            let delay = RESTART_DELAY
                .checked_mul(2_u32.saturating_pow(doublings))
                .unwrap_or(MAX_RESTART_DELAY)
                .min(MAX_RESTART_DELAY);
            last + delay
        };
        (ready > now).then(|| ready - now)
    }

    fn forget_old(&mut self, now: Instant) {
        while self
            .crashes
            .front()
            .is_some_and(|crash| now.duration_since(*crash) >= RESTART_WINDOW)
        {
            self.crashes.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restarts_wait_longer_after_each_crash() {
        let start = Instant::now();
        let mut restarts = Restarts::default();
        assert_eq!(restarts.wait(start), None, "nothing crashed yet");
        restarts.crashed(start);
        assert_eq!(restarts.wait(start), Some(RESTART_DELAY));
        assert_eq!(restarts.wait(start + RESTART_DELAY), None);
        restarts.crashed(start);
        assert_eq!(restarts.wait(start), Some(RESTART_DELAY * 2));
        restarts.crashed(start);
        assert_eq!(restarts.wait(start), Some(RESTART_DELAY * 4));
    }

    #[test]
    fn restart_waits_are_capped() {
        let start = Instant::now();
        let mut restarts = Restarts::default();
        for crash in 0..MAX_CRASHES - 1 {
            restarts.crashed(start + Duration::from_secs(crash as u64));
        }
        let last = start + Duration::from_secs((MAX_CRASHES - 2) as u64);
        let wait = restarts.wait(last).unwrap();
        assert!(wait <= MAX_RESTART_DELAY, "{wait:?}");
    }

    #[test]
    fn too_many_crashes_keep_the_worker_down_for_the_window() {
        let start = Instant::now();
        let mut restarts = Restarts::default();
        for _ in 0..MAX_CRASHES {
            restarts.crashed(start);
        }
        assert_eq!(
            restarts.wait(start + MAX_RESTART_DELAY),
            Some(RESTART_WINDOW - MAX_RESTART_DELAY)
        );
        assert_eq!(restarts.wait(start + RESTART_WINDOW), None);
    }
}
