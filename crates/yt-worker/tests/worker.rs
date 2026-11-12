//! The real `ytu-worker` process, through its client and through its raw
//! protocol. The tests that need ONNX Runtime, the models or the fusion
//! generator are ignored; run them with `YTU_ONNXRUNTIME=<library>
//! YTU_SA3_PROGRAM=<sa3-generate> YTU_SA3_MODELS=<folder> cargo test -p
//! yt-worker -- --ignored`.

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use yt_core::MediaOrigin;
use yt_worker::{
    fusion::{Generator, FUSION_RATE},
    protocol::{
        decode, encode, frame_length, Action, Failure, FusionRequest, FusionSide, JobId, Outcome,
        Request, WorkerMessage, FRAME_HEADER_BYTES, PROTOCOL_VERSION,
    },
    ClientError, Device, WorkerClient, WorkerConfig, BEAT_MODEL_FILENAME, EXIT_CONFIGURATION,
    IDLE_EXIT,
};

const WORKER: &str = env!("CARGO_BIN_EXE_ytu-worker");
const RATE: u32 = 44_100;

/// A configuration whose runtime and model are placeholder files: they pass
/// the worker's start-up check, and loading them fails.
fn placeholder_config(folder: &Path) -> WorkerConfig {
    let placeholder = folder.join("placeholder.bin");
    std::fs::write(&placeholder, b"not a library").unwrap();
    WorkerConfig {
        executable: WORKER.into(),
        runtime_library: placeholder.clone(),
        beat_model: placeholder,
        vocal_model: None,
        fusion: None,
        model_cache: folder.join("cache"),
        idle_exit: Duration::from_secs(1),
    }
}

/// `samples` as 16-bit mono WAV.
fn write_wav(path: &Path, samples: &[f32]) {
    let data = u32::try_from(samples.len() * 2).unwrap();
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&RATE.to_le_bytes());
    bytes.extend_from_slice(&(RATE * 2).to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

/// `seconds` of a drum pattern at `bpm`: a kick on every beat, louder on the
/// first of each bar of four, and a hi-hat between beats.
fn drums(seconds: f32, bpm: f32) -> Vec<f32> {
    let beat = 60.0 / bpm;
    let mut noise = 0x1234_5678_u32;
    (0..(seconds * RATE as f32) as usize)
        .map(|index| {
            let time = index as f32 / RATE as f32;
            let beats = time / beat;
            let since_beat = (beats.fract()) * beat;
            let accent = if (beats as usize).is_multiple_of(4) {
                1.0
            } else {
                0.6
            };
            let kick = accent
                * (-since_beat * 30.0).exp()
                * (std::f32::consts::TAU * (50.0 + 60.0 * (-since_beat * 40.0).exp()) * since_beat)
                    .sin();
            let since_half = ((beats + 0.5).fract()) * beat;
            noise ^= noise << 13;
            noise ^= noise >> 17;
            noise ^= noise << 5;
            let white = noise as f32 / u32::MAX as f32 * 2.0 - 1.0;
            let hat = 0.15 * (-since_half * 120.0).exp() * white;
            0.8 * kick + hat
        })
        .collect()
}

/// A fusion from `outgoing` at 6 s into `incoming`, whose deck starts at
/// 4 s, written to `output`.
fn fusion_request(
    origin: MediaOrigin,
    outgoing: &Path,
    incoming: &Path,
    output: PathBuf,
) -> FusionRequest {
    let side = |path: &Path, seconds: u64| FusionSide {
        origin,
        path: path.to_path_buf(),
        frame: u64::from(RATE) * seconds,
    };
    FusionRequest {
        outgoing: side(outgoing, 6),
        incoming: side(incoming, 4),
        speed: 1.0,
        fusion: yt_mix::Fusion {
            before: 4.0,
            length: 2.0,
            after: 4.0,
            seam: 0.25,
            strength: 0.5,
        },
        tempo_bpm: 120.0,
        key: None,
        seed: 3,
        output,
    }
}

fn read_message(output: &mut impl Read) -> WorkerMessage {
    let mut header = [0_u8; FRAME_HEADER_BYTES];
    output.read_exact(&mut header).unwrap();
    let mut body = vec![0_u8; frame_length(header).unwrap()];
    output.read_exact(&mut body).unwrap();
    decode(&body).unwrap()
}

#[tokio::test]
async fn vocals_without_a_vocal_model_are_unsupported_and_the_worker_lives_on() {
    let folder = tempfile::tempdir().unwrap();
    let audio = folder.path().join("drums.wav");
    write_wav(&audio, &drums(6.0, 120.0));
    let client = WorkerClient::new(placeholder_config(folder.path()));
    let cancel = CancellationToken::new();
    for _ in 0..2 {
        let result = client
            .measure_vocals(MediaOrigin::Library, audio.clone(), &cancel)
            .await;
        assert!(
            matches!(&result, Err(ClientError::Unavailable(reason)) if reason.contains("vocal")),
            "{result:?}"
        );
    }
    let analysis = client
        .analyze(
            MediaOrigin::Library,
            folder.path().join("missing.flac"),
            &cancel,
        )
        .await;
    assert!(
        matches!(
            analysis,
            Err(ClientError::Failed {
                failure: Failure::Audio,
                ..
            })
        ),
        "analysis still runs: {analysis:?}"
    );
}

#[tokio::test]
async fn separating_needs_a_new_output_and_a_bounded_window_for_any_source() {
    let folder = tempfile::tempdir().unwrap();
    let audio = folder.path().join("drums.wav");
    write_wav(&audio, &drums(6.0, 120.0));
    let client = WorkerClient::new(placeholder_config(folder.path()));
    let cancel = CancellationToken::new();
    let output = folder.path().join("vocals.f32");
    std::fs::write(&output, b"taken").unwrap();
    for (frames, output) in [
        (0..RATE.into(), output.clone()),
        (100..100, folder.path().join("empty.f32")),
    ] {
        let refused = client
            .separate_vocals(MediaOrigin::Library, audio.clone(), frames, output, &cancel)
            .await;
        assert!(
            matches!(
                refused,
                Err(ClientError::Failed {
                    failure: Failure::Protocol,
                    ..
                })
            ),
            "{refused:?}"
        );
    }
    assert_eq!(std::fs::read(&output).unwrap(), b"taken", "never replaced");
    // Online audio passes the rights check and only lacks the model here.
    let online = client
        .separate_vocals(
            MediaOrigin::YouTube,
            audio,
            0..RATE.into(),
            folder.path().join("online.f32"),
            &cancel,
        )
        .await;
    assert!(
        matches!(&online, Err(ClientError::Unavailable(reason)) if reason.contains("vocal")),
        "{online:?}"
    );
}

#[tokio::test]
async fn unreadable_audio_fails_before_any_model_is_loaded() {
    let folder = tempfile::tempdir().unwrap();
    let client = WorkerClient::new(placeholder_config(folder.path()));
    let result = client
        .analyze(
            MediaOrigin::Library,
            folder.path().join("missing.flac"),
            &CancellationToken::new(),
        )
        .await;
    assert!(
        matches!(
            result,
            Err(ClientError::Failed {
                failure: Failure::Audio,
                ..
            })
        ),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_device_that_cannot_load_the_runtime_is_unavailable_from_then_on() {
    let folder = tempfile::tempdir().unwrap();
    let audio = folder.path().join("drums.wav");
    write_wav(&audio, &drums(6.0, 120.0));
    let client = WorkerClient::new(placeholder_config(folder.path()));
    let cancel = CancellationToken::new();
    let first = client
        .analyze(MediaOrigin::Library, audio.clone(), &cancel)
        .await;
    assert!(
        matches!(first, Err(ClientError::Unavailable(_))),
        "{first:?}"
    );
    let again = client
        .analyze(
            MediaOrigin::YouTube,
            folder.path().join("missing.m4a"),
            &cancel,
        )
        .await;
    assert!(
        matches!(again, Err(ClientError::Unavailable(_))),
        "no new worker is started for a device that cannot run it: {again:?}"
    );
}

#[tokio::test]
async fn an_idle_worker_exits_and_the_next_job_starts_another() {
    let folder = tempfile::tempdir().unwrap();
    let client = WorkerClient::new(placeholder_config(folder.path()));
    let cancel = CancellationToken::new();
    for _ in 0..2 {
        let result = client
            .analyze(
                MediaOrigin::Library,
                folder.path().join("missing.wav"),
                &cancel,
            )
            .await;
        assert!(
            matches!(
                result,
                Err(ClientError::Failed {
                    failure: Failure::Audio,
                    ..
                })
            ),
            "{result:?}"
        );
        tokio::time::sleep(Duration::from_millis(2_500)).await;
    }
}

#[tokio::test]
async fn a_missing_worker_or_model_makes_the_worker_unavailable() {
    let folder = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let mut missing_worker = placeholder_config(folder.path());
    missing_worker.executable = folder.path().join("no-worker");
    let mut missing_model = placeholder_config(folder.path());
    missing_model.beat_model = folder.path().join("no-model.onnx");
    for config in [missing_worker, missing_model] {
        let result = WorkerClient::new(config)
            .analyze(
                MediaOrigin::Library,
                folder.path().join("song.wav"),
                &cancel,
            )
            .await;
        assert!(
            matches!(result, Err(ClientError::Unavailable(_))),
            "{result:?}"
        );
    }
}

#[tokio::test]
async fn a_cancelled_request_returns_at_once() {
    let folder = tempfile::tempdir().unwrap();
    let client = WorkerClient::new(placeholder_config(folder.path()));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = client
        .analyze(
            MediaOrigin::Library,
            folder.path().join("song.wav"),
            &cancel,
        )
        .await;
    assert_eq!(result, Err(ClientError::Cancelled));
}

#[test]
fn the_worker_refuses_other_protocol_versions_and_exits_when_its_input_closes() {
    let folder = tempfile::tempdir().unwrap();
    let config = placeholder_config(folder.path());
    let mut child = Command::new(WORKER)
        .arg("--runtime")
        .arg(&config.runtime_library)
        .arg("--beat-model")
        .arg(&config.beat_model)
        .arg("--model-cache")
        .arg(&config.model_cache)
        .args(["--idle-seconds", "60"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    match read_message(&mut output) {
        WorkerMessage::Ready {
            protocol_version, ..
        } => assert_eq!(protocol_version, PROTOCOL_VERSION),
        other => panic!("expected the announcement, got {other:?}"),
    }
    let mut request = Request::new(
        JobId(9),
        Action::AnalyzeTrack {
            origin: MediaOrigin::Library,
            path: folder.path().join("song.wav"),
        },
    );
    request.protocol_version = PROTOCOL_VERSION + 1;
    input.write_all(&encode(&request).unwrap()).unwrap();
    input.flush().unwrap();
    match read_message(&mut output) {
        WorkerMessage::Reply(reply) => {
            assert_eq!(reply.job, JobId(9));
            assert!(
                matches!(
                    reply.outcome,
                    Outcome::Failed {
                        failure: Failure::Protocol,
                        ..
                    }
                ),
                "{reply:?}"
            );
        }
        other => panic!("expected a reply, got {other:?}"),
    }
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[test]
fn missing_files_stop_the_worker_with_a_configuration_error() {
    let status = Command::new(WORKER)
        .args([
            "--runtime",
            "/nonexistent/runtime",
            "--beat-model",
            "/nonexistent/model",
            "--model-cache",
            "/nonexistent/cache",
            "--idle-seconds",
            "1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(EXIT_CONFIGURATION));
}

#[tokio::test]
#[ignore = "needs ONNX Runtime (YTU_ONNXRUNTIME) and the beat model"]
async fn drums_are_analyzed_on_the_gpu_and_an_analysis_can_be_cancelled() {
    let runtime = PathBuf::from(std::env::var_os("YTU_ONNXRUNTIME").expect("YTU_ONNXRUNTIME"));
    let folder = tempfile::tempdir().unwrap();
    let audio = folder.path().join("drums.wav");
    write_wav(&audio, &drums(60.0, 120.0));
    let client = WorkerClient::new(WorkerConfig {
        executable: WORKER.into(),
        runtime_library: runtime,
        beat_model: Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../yt-mir/models")
            .join(BEAT_MODEL_FILENAME),
        vocal_model: None,
        fusion: None,
        model_cache: folder.path().join("cache"),
        idle_exit: IDLE_EXIT,
    });
    for pass in [
        "first, compiling the model",
        "second, with the model loaded",
    ] {
        let started = std::time::Instant::now();
        let analyzed = client
            .analyze(
                MediaOrigin::Library,
                audio.clone(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        eprintln!("{pass}: {:.1} s", started.elapsed().as_secs_f64());
        assert_eq!(analyzed.device, Device::Gpu);
        let tempo = analyzed.analysis.tempo_bpm.unwrap();
        assert!((tempo - 120.0).abs() < 1.0, "tempo {tempo}");
    }
    let cancel = CancellationToken::new();
    let (result, ()) = tokio::join!(
        client.analyze(MediaOrigin::Library, audio, &cancel),
        async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            cancel.cancel();
        }
    );
    assert!(
        matches!(result, Err(ClientError::Cancelled) | Ok(_)),
        "{result:?}"
    );
}

#[tokio::test]
#[ignore = "needs ONNX Runtime (YTU_ONNXRUNTIME) and the vocal model"]
async fn vocals_are_measured_and_a_window_separated_on_the_gpu() {
    let runtime = PathBuf::from(std::env::var_os("YTU_ONNXRUNTIME").expect("YTU_ONNXRUNTIME"));
    let folder = tempfile::tempdir().unwrap();
    let audio = folder.path().join("drums.wav");
    write_wav(&audio, &drums(20.0, 120.0));
    let models = Path::new(env!("CARGO_MANIFEST_DIR")).join("../yt-mir/models");
    let client = WorkerClient::new(WorkerConfig {
        executable: WORKER.into(),
        runtime_library: runtime,
        beat_model: models.join(BEAT_MODEL_FILENAME),
        vocal_model: Some(models.join(yt_worker::VOCAL_MODEL_FILENAME)),
        fusion: None,
        model_cache: folder.path().join("cache"),
        idle_exit: IDLE_EXIT,
    });
    let cancel = CancellationToken::new();
    let started = std::time::Instant::now();
    let vocals = client
        .measure_vocals(MediaOrigin::Library, audio.clone(), &cancel)
        .await
        .unwrap();
    eprintln!(
        "measured in {:.1} s on {:?}",
        started.elapsed().as_secs_f64(),
        vocals.device
    );
    assert_eq!(vocals.device, Device::Gpu);
    assert_eq!(vocals.vocal_db.len(), 40, "one level every half second");
    let output = folder.path().join("window.f32");
    let window = u64::from(RATE) * 5..u64::from(RATE) * 9;
    let separated = client
        .separate_vocals(
            MediaOrigin::Library,
            audio,
            window.clone(),
            output.clone(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!((separated.channels, separated.sample_rate), (1, RATE));
    let bytes = std::fs::read(&output).unwrap();
    assert_eq!(bytes.len() as u64, (window.end - window.start) * 4);
}

#[tokio::test]
async fn fusions_need_their_model_for_either_source() {
    let folder = tempfile::tempdir().unwrap();
    let audio = folder.path().join("drums.wav");
    write_wav(&audio, &drums(12.0, 120.0));
    let client = WorkerClient::new(placeholder_config(folder.path()));
    let cancel = CancellationToken::new();
    let output = folder.path().join("fusion.f32");
    // Both sources allow fusing (YouTube by the owner's choice of
    // 12 November 2026); without the model, neither can.
    for origin in [MediaOrigin::YouTube, MediaOrigin::Library] {
        let result = client
            .render_fusion(
                fusion_request(origin, &audio, &audio, output.clone()),
                &cancel,
            )
            .await;
        assert!(
            matches!(&result, Err(ClientError::Unavailable(reason)) if reason.contains("fusion")),
            "{result:?}"
        );
    }
    assert!(!output.exists());
    let analysis = client
        .analyze(
            MediaOrigin::Library,
            folder.path().join("missing.flac"),
            &cancel,
        )
        .await;
    assert!(
        matches!(
            analysis,
            Err(ClientError::Failed {
                failure: Failure::Audio,
                ..
            })
        ),
        "analysis still runs: {analysis:?}"
    );
}

#[tokio::test]
#[ignore = "needs ONNX Runtime (YTU_ONNXRUNTIME) and the fusion generator (YTU_SA3_PROGRAM, YTU_SA3_MODELS)"]
async fn a_fusion_is_rendered_on_the_gpu_from_two_songs() {
    let variable = |name: &str| PathBuf::from(std::env::var_os(name).expect(name));
    let generator = Generator::find(variable("YTU_SA3_PROGRAM"), variable("YTU_SA3_MODELS"))
        .expect("the generator and its weights");
    let folder = tempfile::tempdir().unwrap();
    let (outgoing, incoming) = (folder.path().join("a.wav"), folder.path().join("b.wav"));
    write_wav(&outgoing, &drums(12.0, 120.0));
    write_wav(&incoming, &drums(12.0, 120.0));
    let models = Path::new(env!("CARGO_MANIFEST_DIR")).join("../yt-mir/models");
    let client = WorkerClient::new(WorkerConfig {
        executable: WORKER.into(),
        runtime_library: variable("YTU_ONNXRUNTIME"),
        beat_model: models.join(BEAT_MODEL_FILENAME),
        vocal_model: None,
        fusion: Some(generator),
        model_cache: folder.path().join("cache"),
        idle_exit: IDLE_EXIT,
    });
    let output = folder.path().join("fusion.f32");
    let request = fusion_request(MediaOrigin::Library, &outgoing, &incoming, output.clone());
    let expected = request.fusion.clip_frames(FUSION_RATE);
    let started = std::time::Instant::now();
    let frames = client
        .render_fusion(request, &CancellationToken::new())
        .await
        .unwrap();
    eprintln!("rendered in {:.1} s", started.elapsed().as_secs_f64());
    assert_eq!(frames, expected);
    let bytes = std::fs::read(&output).unwrap();
    assert_eq!(bytes.len() as u64, frames * 2 * 4, "stereo f32");
    let peak = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|sample| f32::from_le_bytes(*sample).abs())
        .fold(0.0_f32, f32::max);
    assert!(peak > 0.01 && peak < 4.0, "the clip holds audio: {peak}");
}
