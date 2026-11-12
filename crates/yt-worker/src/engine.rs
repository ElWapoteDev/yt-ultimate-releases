//! Runs one job: checks the source's rights and the governor, decodes the
//! audio, loads the model the job needs the first time it is needed, and runs
//! it under the governor's pace. An AI fusion runs the generative model in
//! its own process in one burst; the rest it owes the duty cycle is taken
//! before the next job, so its clip is not delayed.

use crate::platform;
use rodio::Source;
use std::{
    cell::Cell,
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};
use yt_core::{SourceRights, TrackAnalysis};
use yt_mir::{Analyzer, BeatModel, Device, MirError, Pace, Pcm, VocalModel};
use yt_worker::{
    fusion::{self, Audio, Canvas, FusionError, Generator},
    governor::{self, Conditions, Verdict},
    protocol::{Action, DeferReason, Failure, FusionRequest, JobId, Outcome, Reply, Request},
};

/// Longest audio analyzed, in seconds; longer media keeps the classic
/// crossfade. Bounds the decoded samples held in memory to about 350 MB
/// (stereo at 48 kHz).
const MAX_TRACK_SECONDS: u64 = 15 * 60;
/// Samples decoded between two cancellation checks.
const CANCEL_CHECK_SAMPLES: usize = 1 << 16;
/// How long the governor trusts one reading of the power and thermal state.
const PROBE_INTERVAL: Duration = Duration::from_secs(1);
/// Longest single sleep while resting, so a cancellation is seen promptly.
const REST_SLICE: Duration = Duration::from_millis(50);
/// Sampling steps of an AI fusion: Stable Audio 3's distilled schedule.
const FUSION_STEPS: u32 = 8;

/// The files the worker loads its models from.
pub(crate) struct Models {
    pub runtime: PathBuf,
    pub beat: PathBuf,
    /// Without it, vocals are never measured.
    pub vocal: Option<PathBuf>,
    /// Without it, no fusion is rendered.
    pub fusion: Option<Generator>,
    /// Folder where CoreML keeps compiled models.
    pub cache: PathBuf,
}

/// A model loaded on the first job that needs it, or why it cannot load.
enum Slot<T> {
    Empty,
    Loaded(T),
    Failed(String),
}

impl<T> Slot<T> {
    /// The model, loading it with `load` the first time; a model that cannot
    /// load stays so. A native runtime that panics while loading must not
    /// take the worker down in a restart loop: it counts as a failure.
    fn get(&mut self, load: impl FnOnce() -> Result<T, String>) -> Result<&mut T, String> {
        if matches!(self, Self::Empty) {
            *self = match panic::catch_unwind(AssertUnwindSafe(load)) {
                Ok(Ok(model)) => Self::Loaded(model),
                Ok(Err(error)) => Self::Failed(error),
                Err(_) => Self::Failed("the model runtime failed while loading".into()),
            };
        }
        match self {
            Self::Loaded(model) => Ok(model),
            Self::Failed(error) => Err(error.clone()),
            Self::Empty => Err("the model is not loaded".into()),
        }
    }
}

/// Runs jobs with the models it loads on demand.
pub(crate) struct Engine {
    models: Models,
    analyzer: Slot<Analyzer>,
    vocals: Slot<VocalModel>,
    /// When the last fusion finished and the rest it owes the duty cycle.
    rest_due: Option<(Instant, Duration)>,
}

struct Decoded {
    samples: Vec<f32>,
    channels: u16,
    sample_rate: u32,
}

impl Decoded {
    fn audio(&self) -> Audio<'_> {
        Audio {
            samples: &self.samples,
            channels: self.channels,
            rate: self.sample_rate,
        }
    }
}

enum DecodeError {
    Cancelled,
    Failed(String),
}

impl Engine {
    pub(crate) fn new(models: Models) -> Self {
        Self {
            models,
            analyzer: Slot::Empty,
            vocals: Slot::Empty,
            rest_due: None,
        }
    }

    /// The reply to `request`; `cancel` stops it.
    pub(crate) fn run(&mut self, request: Request, cancel: &AtomicBool) -> Reply {
        let job = request.job;
        if !self.rest(cancel) {
            return Reply::new(job, Outcome::Cancelled);
        }
        let (origin, path, work) = match request.action {
            Action::AnalyzeTrack { origin, path } => (origin, path, Work::Analyze),
            Action::MeasureVocals { origin, path } => (origin, path, Work::Measure),
            Action::SeparateVocals {
                origin,
                path,
                first_frame,
                end_frame,
                output,
            } => (
                origin,
                path,
                Work::Separate {
                    frames: first_frame..end_frame,
                    output,
                },
            ),
            Action::RenderFusion(request) => return self.fusion(job, request, cancel),
            Action::Cancel => {
                return Reply::failed(job, Failure::Protocol, "a cancellation is not a job")
            }
        };
        let rights = SourceRights::of(origin);
        // Measuring keeps nothing derived from the audio, so it is analysis;
        // separating for playback needs its own right.
        let allowed = match &work {
            Work::Separate { .. } => rights.separate,
            Work::Analyze | Work::Measure => rights.analyze,
        };
        if !allowed {
            return Reply::failed(job, Failure::Rights, "this source does not allow the job");
        }
        if let Work::Separate { frames, output } = &work {
            if let Err(detail) = check_window(frames, output) {
                return Reply::failed(job, Failure::Protocol, detail);
            }
        }
        let governor = Governor::new(cancel);
        let conditions = match governor.check() {
            Ok(conditions) => conditions,
            Err(reason) => return Reply::new(job, Outcome::Deferred { reason }),
        };
        platform::set_thread_priority(conditions.power);
        let decoded = match decode(&path, cancel) {
            Ok(decoded) => decoded,
            Err(DecodeError::Cancelled) => return Reply::new(job, Outcome::Cancelled),
            Err(DecodeError::Failed(detail)) => return Reply::failed(job, Failure::Audio, &detail),
        };
        let pcm = match Pcm::new(&decoded.samples, decoded.channels, decoded.sample_rate) {
            Ok(pcm) => pcm,
            Err(error) => return Reply::failed(job, Failure::Audio, &error.to_string()),
        };
        let measured = match work {
            Work::Analyze => self.analyze(&pcm, &governor),
            Work::Measure => self.measure_vocals(&pcm, &governor),
            Work::Separate { frames, output } => {
                self.separate_vocals(&pcm, frames, &output, &governor)
            }
        };
        match measured {
            Ok(outcome) => Reply::new(job, outcome),
            Err(Error::Unsupported(detail)) => Reply::failed(job, Failure::Unsupported, &detail),
            Err(Error::Mir(MirError::Cancelled)) => Reply::new(
                job,
                match governor.deferred.get() {
                    Some(reason) => Outcome::Deferred { reason },
                    None => Outcome::Cancelled,
                },
            ),
            Err(Error::Mir(MirError::InvalidAudio(detail))) => {
                Reply::failed(job, Failure::Audio, &detail)
            }
            Err(Error::Mir(error)) => Reply::failed(job, Failure::Runtime, &error.to_string()),
            Err(Error::Output(detail)) => Reply::failed(job, Failure::Runtime, &detail),
        }
    }

    fn analyze(&mut self, pcm: &Pcm<'_>, governor: &Governor<'_>) -> Result<Outcome, Error> {
        let models = &self.models;
        let analyzer = self
            .analyzer
            .get(|| load_analyzer(models))
            .map_err(Error::Unsupported)?;
        let device = analyzer.model().device();
        let analysis = analyzer.analyze(pcm, governor).map_err(Error::Mir)?;
        Ok(Outcome::Analysis {
            analysis: Box::new(analysis),
            device,
        })
    }

    fn measure_vocals(&mut self, pcm: &Pcm<'_>, governor: &Governor<'_>) -> Result<Outcome, Error> {
        let models = &self.models;
        let model = self
            .vocals
            .get(|| load_vocals(models))
            .map_err(Error::Unsupported)?;
        let device = model.device();
        let vocal_db = model.measure(pcm, governor).map_err(Error::Mir)?;
        Ok(Outcome::Vocals { vocal_db, device })
    }

    fn separate_vocals(
        &mut self,
        pcm: &Pcm<'_>,
        frames: std::ops::Range<u64>,
        output: &Path,
        governor: &Governor<'_>,
    ) -> Result<Outcome, Error> {
        let models = &self.models;
        let model = self
            .vocals
            .get(|| load_vocals(models))
            .map_err(Error::Unsupported)?;
        let vocals = model
            .separate(pcm, frames.clone(), governor)
            .map_err(Error::Mir)?;
        write_samples(output, &vocals).map_err(Error::Output)?;
        Ok(Outcome::Separated {
            frames: frames.end - frames.start,
            channels: pcm.channels(),
            sample_rate: pcm.sample_rate(),
        })
    }
}

impl Engine {
    /// Rest what the last fusion still owes the duty cycle; `false` when
    /// cancelled meanwhile.
    fn rest(&mut self, cancel: &AtomicBool) -> bool {
        let Some((finished, rest)) = self.rest_due.take() else {
            return true;
        };
        let mut left = rest.saturating_sub(finished.elapsed());
        while !left.is_zero() {
            if cancel.load(Ordering::Relaxed) {
                self.rest_due = Some((finished, rest));
                return false;
            }
            let slice = left.min(REST_SLICE);
            thread::sleep(slice);
            left -= slice;
        }
        true
    }

    /// Render an AI fusion and write its clip to the job's output.
    fn fusion(&mut self, job: JobId, request: FusionRequest, cancel: &AtomicBool) -> Reply {
        let FusionRequest {
            outgoing,
            incoming,
            speed,
            fusion,
            tempo_bpm,
            key,
            seed,
            output,
        } = request;
        // The model is conditioned on both songs' audio.
        if !(SourceRights::of(outgoing.origin).generate
            && SourceRights::of(incoming.origin).generate)
        {
            return Reply::failed(job, Failure::Rights, "this source does not allow the job");
        }
        let valid = fusion.validate().is_ok()
            && speed.is_finite()
            && fusion::FUSION_SPEEDS.contains(&speed)
            && TrackAnalysis::TEMPO_BPM.contains(&tempo_bpm);
        if !valid {
            return Reply::failed(job, Failure::Protocol, "the fusion is out of bounds");
        }
        if let Err(detail) = check_output(&output) {
            return Reply::failed(job, Failure::Protocol, detail);
        }
        let Some(generator) = self.models.fusion.clone() else {
            return Reply::failed(job, Failure::Unsupported, "no fusion model is installed");
        };
        let governor = Governor::new(cancel);
        let conditions = match governor.check() {
            Ok(conditions) => conditions,
            Err(reason) => return Reply::new(job, Outcome::Deferred { reason }),
        };
        platform::set_thread_priority(conditions.power);
        let mut decoded = Vec::with_capacity(2);
        for side in [&outgoing, &incoming] {
            match decode(&side.path, cancel) {
                Ok(audio) => decoded.push(audio),
                Err(DecodeError::Cancelled) => return Reply::new(job, Outcome::Cancelled),
                Err(DecodeError::Failed(detail)) => {
                    return Reply::failed(job, Failure::Audio, &detail)
                }
            }
        }
        let canvas = match Canvas::new(
            decoded[0].audio(),
            outgoing.frame,
            decoded[1].audio(),
            incoming.frame,
            speed,
            fusion,
        ) {
            Ok(canvas) => canvas,
            Err(error) => return Reply::failed(job, Failure::Audio, &error.to_string()),
        };
        drop(decoded);
        let Some(scratch) = output.parent() else {
            return Reply::failed(job, Failure::Protocol, "the output has no folder");
        };
        let started = Instant::now();
        let rendered = generator.render(
            &canvas,
            &fusion::prompt(tempo_bpm, key),
            seed,
            FUSION_STEPS,
            scratch,
            || governor.keep_going(),
        );
        if let Verdict::Run { duty_cycle } = governor.verdict() {
            self.rest_due = Some((
                Instant::now(),
                governor::rest_after(started.elapsed(), duty_cycle),
            ));
        }
        let clip = match rendered {
            Ok(clip) => clip,
            Err(FusionError::Cancelled) => {
                return Reply::new(
                    job,
                    match governor.deferred.get() {
                        Some(reason) => Outcome::Deferred { reason },
                        None => Outcome::Cancelled,
                    },
                )
            }
            Err(error @ (FusionError::Layout(_) | FusionError::Audio(_))) => {
                return Reply::failed(job, Failure::Audio, &error.to_string())
            }
            Err(error @ FusionError::Model(_)) => {
                return Reply::failed(job, Failure::Runtime, &error.to_string())
            }
        };
        if let Err(detail) = write_samples(&output, &clip) {
            return Reply::failed(job, Failure::Runtime, &detail);
        }
        Reply::new(
            job,
            Outcome::Fused {
                frames: (clip.len() / usize::from(fusion::FUSION_CHANNELS)) as u64,
            },
        )
    }
}

/// Write `samples` as little-endian `f32` to the new file `output`.
fn write_samples(output: &Path, samples: &[f32]) -> Result<(), String> {
    let bytes: Vec<u8> = samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect();
    // `create_new` never replaces a file: the output must be new.
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .and_then(|mut file| std::io::Write::write_all(&mut file, &bytes))
        .map_err(|error| format!("the audio cannot be written: {}", error.kind()))
}

/// What a job does with the decoded audio.
enum Work {
    Analyze,
    Measure,
    Separate {
        frames: std::ops::Range<u64>,
        output: PathBuf,
    },
}

/// Why a job's model brought no result.
enum Error {
    /// The model cannot load on this device.
    Unsupported(String),
    Mir(MirError),
    /// The result could not be written.
    Output(String),
}

/// `Err` unless `frames` is a non-empty window no longer than a stem
/// (`yt_media::MAX_STEM_SECONDS` at the highest rate) and `output` names a
/// new file in an existing folder.
fn check_window(frames: &std::ops::Range<u64>, output: &Path) -> Result<(), &'static str> {
    let longest = u64::from(yt_media::MAX_STEM_SECONDS)
        * u64::from(*yt_core::TrackAnalysis::SAMPLE_RATE.end());
    if frames.start >= frames.end || frames.end - frames.start > longest {
        return Err("the window is empty or too long");
    }
    check_output(output)
}

/// `Err` unless `output` names a new file in an existing folder.
fn check_output(output: &Path) -> Result<(), &'static str> {
    let folder = output.parent().filter(|folder| folder.is_dir());
    if folder.is_none() || output.exists() {
        return Err("the output must be a new file in an existing folder");
    }
    Ok(())
}

/// CoreML's cache folder, when it can be created: without it CoreML compiles
/// the models again on every start.
fn cache(models: &Models) -> Option<&Path> {
    std::fs::create_dir_all(&models.cache)
        .is_ok()
        .then_some(models.cache.as_path())
}

/// The analyzer on the GPU, or on the CPU when this device's GPU cannot run
/// the beat model.
fn load_analyzer(models: &Models) -> Result<Analyzer, String> {
    yt_mir::init_runtime(&models.runtime).map_err(|error| error.to_string())?;
    let model = BeatModel::load(&models.beat, Device::Gpu, cache(models)).or_else(|gpu| {
        BeatModel::load(&models.beat, Device::Cpu, None)
            .map_err(|cpu| format!("GPU: {gpu}; CPU: {cpu}"))
    })?;
    Ok(Analyzer::new(model))
}

/// The vocal model on the GPU only: on the CPU it would take minutes of full
/// load per song and heat the device, so the DJ does without vocals there.
fn load_vocals(models: &Models) -> Result<VocalModel, String> {
    let path = models
        .vocal
        .as_ref()
        .ok_or_else(|| String::from("no vocal model is installed"))?;
    yt_mir::init_runtime(&models.runtime).map_err(|error| error.to_string())?;
    VocalModel::load(path, Device::Gpu, cache(models)).map_err(|error| error.to_string())
}

/// The decoded interleaved samples of the file at `path`, refused beyond
/// [`MAX_TRACK_SECONDS`]. Messages never include the path.
fn decode(path: &Path, cancel: &AtomicBool) -> Result<Decoded, DecodeError> {
    let failed = |detail: String| DecodeError::Failed(detail);
    let metadata = std::fs::metadata(path).map_err(|error| failed(error.to_string()))?;
    if !metadata.is_file() {
        return Err(failed("the audio is not a regular file".into()));
    }
    let source = yt_media::decode_file(path).map_err(|error| failed(error.to_string()))?;
    let (channels, sample_rate) = (source.channels(), source.sample_rate());
    if channels == 0 || sample_rate == 0 {
        return Err(failed("the audio has no channels or no sample rate".into()));
    }
    let per_second = u64::from(sample_rate) * u64::from(channels);
    let limit = usize::try_from(MAX_TRACK_SECONDS * per_second).unwrap_or(usize::MAX);
    let expected = source
        .total_duration()
        .map(|duration| (duration.as_secs_f64() * per_second as f64) as usize)
        .unwrap_or(0)
        .min(limit);
    let mut samples = Vec::with_capacity(expected);
    for sample in source {
        if samples.len() % CANCEL_CHECK_SAMPLES == 0 && cancel.load(Ordering::Relaxed) {
            return Err(DecodeError::Cancelled);
        }
        if samples.len() == limit {
            return Err(failed(format!(
                "the audio is longer than {} minutes",
                MAX_TRACK_SECONDS / 60
            )));
        }
        samples.push(sample);
    }
    Ok(Decoded {
        samples,
        channels,
        sample_rate,
    })
}

/// The power governor applied to one job: reads the conditions at most once
/// per [`PROBE_INTERVAL`], rests after each piece of work so the duty cycle
/// holds, and stops the job when the verdict becomes [`Verdict::Defer`].
struct Governor<'a> {
    cancel: &'a AtomicBool,
    probed: Cell<Option<(Instant, Verdict)>>,
    deferred: Cell<Option<DeferReason>>,
}

impl<'a> Governor<'a> {
    fn new(cancel: &'a AtomicBool) -> Self {
        Self {
            cancel,
            probed: Cell::new(None),
            deferred: Cell::new(None),
        }
    }

    /// The conditions now, or why work has to wait.
    fn check(&self) -> Result<Conditions, DeferReason> {
        let conditions = platform::conditions();
        let verdict = governor::verdict(conditions);
        self.probed.set(Some((Instant::now(), verdict)));
        match verdict {
            Verdict::Run { .. } => Ok(conditions),
            Verdict::Defer(reason) => Err(reason),
        }
    }

    /// Whether a running model may go on: not cancelled and not held back
    /// by the power or temperature.
    fn keep_going(&self) -> bool {
        if self.cancel.load(Ordering::Relaxed) {
            return false;
        }
        match self.verdict() {
            Verdict::Run { .. } => true,
            Verdict::Defer(reason) => {
                self.deferred.set(Some(reason));
                false
            }
        }
    }

    fn verdict(&self) -> Verdict {
        match self.probed.get() {
            Some((at, verdict)) if at.elapsed() < PROBE_INTERVAL => verdict,
            _ => {
                let verdict = governor::verdict(platform::conditions());
                self.probed.set(Some((Instant::now(), verdict)));
                verdict
            }
        }
    }
}

impl Pace for Governor<'_> {
    fn proceed(&self, busy: Duration) -> bool {
        if self.cancel.load(Ordering::Relaxed) {
            return false;
        }
        let duty_cycle = match self.verdict() {
            Verdict::Run { duty_cycle } => duty_cycle,
            Verdict::Defer(reason) => {
                self.deferred.set(Some(reason));
                return false;
            }
        };
        let mut rest = governor::rest_after(busy, duty_cycle);
        while !rest.is_zero() {
            if self.cancel.load(Ordering::Relaxed) {
                return false;
            }
            let slice = rest.min(REST_SLICE);
            thread::sleep(slice);
            rest -= slice;
        }
        true
    }
}
