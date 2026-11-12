//! `ytu-worker`: the DJ's compute process (`docs/AI_DJ.md` §5.8). Announces
//! itself, reads framed requests on stdin, runs one job at a time at low
//! priority under the power governor, and writes one framed reply per job on
//! stdout. Exits when stdin closes or after the idle time without jobs.

mod engine;
mod platform;

use std::{
    ffi::OsString,
    io::{self, Read, Write},
    path::PathBuf,
    process::ExitCode,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, PoisonError,
    },
    thread,
    time::Duration,
};
use yt_worker::{
    protocol::{
        decode, encode, frame_length, request_job, Action, Failure, JobId, Reply, Request,
        WorkerMessage, FRAME_HEADER_BYTES, PROTOCOL_VERSION,
    },
    ANALYZER_VERSION, EXIT_CONFIGURATION,
};

/// Longest idle time accepted on the command line, in seconds.
const MAX_IDLE_SECONDS: u64 = 3_600;

/// The job the reader thread handed over last, with its cancellation flag.
type Current = Arc<Mutex<Option<(JobId, Arc<AtomicBool>)>>>;

enum Incoming {
    Job(Request, Arc<AtomicBool>),
    Refused(Reply),
}

/// What the application passes on the command line.
struct Options {
    runtime: PathBuf,
    beat_model: PathBuf,
    /// Optional: without it, vocals are never measured.
    vocal_model: Option<PathBuf>,
    /// Optional: without both, no fusion is rendered.
    fusion_program: Option<PathBuf>,
    fusion_models: Option<PathBuf>,
    model_cache: PathBuf,
    idle: Duration,
}

impl Options {
    /// The options in `args`, or `None` when a required one is missing, one
    /// is unknown or invalid, or a file does not exist.
    fn parse(mut args: impl Iterator<Item = OsString>) -> Option<Self> {
        let (mut runtime, mut beat_model, mut model_cache, mut idle) = (None, None, None, None);
        let (mut vocal_model, mut fusion_program, mut fusion_models) = (None, None, None);
        while let Some(flag) = args.next() {
            let value = args.next()?;
            match flag.to_str()? {
                "--runtime" => runtime = Some(PathBuf::from(value)),
                "--beat-model" => beat_model = Some(PathBuf::from(value)),
                "--vocal-model" => vocal_model = Some(PathBuf::from(value)),
                "--fusion-program" => fusion_program = Some(PathBuf::from(value)),
                "--fusion-models" => fusion_models = Some(PathBuf::from(value)),
                "--model-cache" => model_cache = Some(PathBuf::from(value)),
                "--idle-seconds" => {
                    let seconds: u64 = value.to_str()?.parse().ok()?;
                    if !(1..=MAX_IDLE_SECONDS).contains(&seconds) {
                        return None;
                    }
                    idle = Some(Duration::from_secs(seconds));
                }
                _ => return None,
            }
        }
        let options = Self {
            runtime: runtime?,
            beat_model: beat_model?,
            vocal_model,
            fusion_program,
            fusion_models,
            model_cache: model_cache?,
            idle: idle?,
        };
        let files = options.runtime.is_file()
            && options.beat_model.is_file()
            && options
                .vocal_model
                .as_ref()
                .is_none_or(|path| path.is_file());
        files.then_some(options)
    }
}

fn main() -> ExitCode {
    let Some(options) = Options::parse(std::env::args_os().skip(1)) else {
        return ExitCode::from(u8::try_from(EXIT_CONFIGURATION).unwrap_or(u8::MAX));
    };
    platform::lower_process_priority();
    let mut output = io::stdout().lock();
    let ready = WorkerMessage::Ready {
        protocol_version: PROTOCOL_VERSION,
        analyzer_version: ANALYZER_VERSION,
    };
    if write_message(&mut output, &ready).is_err() {
        return ExitCode::SUCCESS;
    }
    let current = Current::default();
    let (sender, jobs) = mpsc::channel();
    let reader = current.clone();
    thread::spawn(move || read_requests(io::stdin(), sender, reader));
    let mut engine = engine::Engine::new(engine::Models {
        runtime: options.runtime,
        beat: options.beat_model,
        vocal: options.vocal_model,
        // Incomplete fusion files only disable fusions.
        fusion: options
            .fusion_program
            .zip(options.fusion_models)
            .and_then(|(program, models)| yt_worker::fusion::Generator::find(program, models)),
        cache: options.model_cache,
    });
    while let Ok(incoming) = jobs.recv_timeout(options.idle) {
        let reply = match incoming {
            Incoming::Job(request, cancel) => engine.run(request, &cancel),
            Incoming::Refused(reply) => reply,
        };
        if write_message(&mut output, &WorkerMessage::Reply(reply)).is_err() {
            break;
        }
    }
    ExitCode::SUCCESS
}

/// Hand every job to the main loop and apply cancellations, until the
/// application closes the pipe or sends a frame that cannot be read; then
/// stop the running job.
fn read_requests(input: io::Stdin, jobs: mpsc::Sender<Incoming>, current: Current) {
    let mut input = input.lock();
    while let Some(body) = read_frame(&mut input) {
        let request = match decode::<Request>(&body)
            .and_then(|request| request.check_version().map(|()| request))
        {
            Ok(request) => request,
            Err(error) => {
                let Some(job) = request_job(&body) else {
                    continue;
                };
                let refused = Reply::failed(job, Failure::Protocol, &error.to_string());
                if jobs.send(Incoming::Refused(refused)).is_err() {
                    break;
                }
                continue;
            }
        };
        if matches!(request.action, Action::Cancel) {
            let current = current.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((job, cancel)) = current.as_ref() {
                if *job == request.job {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
            continue;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *current.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((request.job, cancel.clone()));
        if jobs.send(Incoming::Job(request, cancel)).is_err() {
            break;
        }
    }
    if let Some((_, cancel)) = current
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        cancel.store(true, Ordering::Relaxed);
    }
}

/// One frame body, or `None` at the end of the input or for a frame that
/// cannot be accepted (the stream cannot be resynchronized after it).
fn read_frame(input: &mut impl Read) -> Option<Vec<u8>> {
    let mut header = [0_u8; FRAME_HEADER_BYTES];
    input.read_exact(&mut header).ok()?;
    let length = frame_length(header).ok()?;
    let mut body = vec![0_u8; length];
    input.read_exact(&mut body).ok()?;
    Some(body)
}

fn write_message(output: &mut impl Write, message: &WorkerMessage) -> io::Result<()> {
    let frame = encode(message).map_err(io::Error::other)?;
    output.write_all(&frame)?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> impl Iterator<Item = OsString> {
        values
            .iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn options_need_every_flag_existing_files_and_a_bounded_idle_time() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        let file = file.to_str().unwrap();
        let cache = folder.path().to_str().unwrap();
        let valid = [
            "--runtime",
            file,
            "--beat-model",
            file,
            "--model-cache",
            cache,
            "--idle-seconds",
            "180",
        ];
        let options = Options::parse(args(&valid)).unwrap();
        assert_eq!(options.idle, Duration::from_secs(180));
        assert!(options.vocal_model.is_none(), "the vocal model is optional");
        let with_vocals = [&valid[..], &["--vocal-model", file]].concat();
        assert!(Options::parse(args(&with_vocals))
            .unwrap()
            .vocal_model
            .is_some());
        let missing_vocals = [&valid[..], &["--vocal-model", "/nonexistent/vocals.onnx"]].concat();
        assert!(Options::parse(args(&missing_vocals)).is_none());
        assert!(Options::parse(args(&valid[..6])).is_none(), "missing flag");
        let mut unbounded = valid;
        unbounded[7] = "0";
        assert!(Options::parse(args(&unbounded)).is_none());
        let mut missing = valid;
        missing[1] = "/nonexistent/runtime";
        assert!(Options::parse(args(&missing)).is_none());
        let mut unknown = valid;
        unknown[6] = "--idle";
        assert!(Options::parse(args(&unknown)).is_none());
    }

    #[test]
    fn a_truncated_or_oversized_frame_ends_the_input() {
        assert!(read_frame(&mut &[1_u8, 0][..]).is_none());
        let oversized = u32::MAX.to_le_bytes();
        assert!(read_frame(&mut &oversized[..]).is_none());
        let frame = encode(&WorkerMessage::Ready {
            protocol_version: 1,
            analyzer_version: 1,
        })
        .unwrap();
        let body = read_frame(&mut frame.as_slice()).unwrap();
        assert_eq!(body.len(), frame.len() - FRAME_HEADER_BYTES);
    }
}
