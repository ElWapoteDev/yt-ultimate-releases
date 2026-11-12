# yt-worker

The DJ's compute worker. Heavy work runs in `ytu-worker`, a separate low-priority process, so a GPU driver fault, a
watchdog reset or a crash in a native runtime never stops playback, and the models' memory returns to the system when
the process exits. It analyzes tracks, measures and separates their vocals (`yt-mir`), and renders AI fusions of two
songs with a generative model ([`docs/AI_DJ.md`](../../docs/AI_DJ.md) §4.4, §5.8–5.9). The same binary is meant to run
behind the future SaaS server.

## API

- `protocol`: the versioned job protocol. Every message is one frame, a little-endian `u32` length and JSON, at most
  `MAX_FRAME_BYTES` (4 MiB). The worker first sends `WorkerMessage::Ready { protocol_version, analyzer_version }`;
  each `Request { protocol_version, job, action }` then gets exactly one `Reply { protocol_version, job, outcome }`.
  Actions (`PROTOCOL_VERSION` 4): `AnalyzeTrack { origin, path }`, `MeasureVocals { origin, path }`,
  `SeparateVocals { origin, path, first_frame, end_frame, output }`, `RenderFusion(FusionRequest { outgoing,
  incoming, speed, fusion, tempo_bpm, key, seed, output })` (each side a `FusionSide { origin, path, frame }`, the
  fusion a `yt_mix::Fusion`) and `Cancel`. Outcomes: `Analysis { analysis, device }`, `Vocals { vocal_db, device }`,
  `Separated { frames, channels, sample_rate }`, `Fused { frames }`, `Cancelled`,
  `Deferred { reason }` (`LowPower`, `Thermal`), `Failed { failure, detail }` (`Protocol`, `Rights`, `Audio`,
  `Unsupported`, `Runtime`; the detail is bounded and never names a path). `encode`, `frame_length`, `decode` and
  `request_job` are the framing both sides share.
- `client::WorkerClient::new(WorkerConfig)` and `analyze(origin, path, &CancellationToken) -> Result<Analyzed,
  ClientError>`: what the application uses. It starts the worker on the first job, waits for `Ready` and checks both
  versions, runs one job at a time, validates every analysis (`TrackAnalysis::validate`, analyzer version) before
  returning it, and asks the worker to cancel when the token fires. `measure_vocals(origin, path, &token) ->
  Result<Vocals, ClientError>` checks every level against `TrackAnalysis::LEVEL_DB` and the count against the
  longest track; `measures_vocals()` says whether the configuration has a vocal model (`WorkerConfig::vocal_model`
  is optional). `separate_vocals(origin, path, frames, output, &token) -> Result<Separated, ClientError>` checks the
  written length against the window; the caller reads the file and removes it. `render_fusion(request, &token) ->
  Result<u64, ClientError>` checks the clip's frames against `Fusion::clip_frames`; `renders_fusions()` says whether
  the configuration has the fusion model (`WorkerConfig::fusion`, an optional `fusion::Generator`).
- `fusion`: the canvas of an AI fusion and the model run that renders it. `Canvas::new(outgoing, exit, incoming,
  entry, speed, fusion)` builds it at `FUSION_RATE` (44.1 kHz, the model's) in stereo: `before` seconds of the
  outgoing song alone, both songs over `length` (the incoming one stretched with `yt_media::stretch`, the decks' own
  time stretch, so it lines up with the incoming deck; equal-power, the outgoing falling), then `after` seconds of the
  incoming song alone. `Canvas::clip(output)` cuts the model's rendering to the fusion plus one seam on each side and
  matches its level to the canvas's (±6 dB). `Generator::find(program, models)` needs sa3.cpp's static
  `sa3-generate` and the five Stable Audio 3 Medium GGUF files (`Generator::MODEL_FILES`); `render(canvas, prompt,
  seed, steps, scratch, keep_going)` runs it audio to audio from the fusion's `strength` (its starting noise), without
  loudness shaping, polls `keep_going` every 50 ms (and kills it when it says stop or after four minutes), and removes
  its files. `prompt(tempo_bpm, key)` describes the fusion to the model.
- `ClientError`: `Unavailable` (missing worker, runtime or model, another version, or a device that cannot load the
  runtime; permanent for the client), `Restarting(wait)`, `Cancelled`, `Deferred`, `Failed`, `Crashed`,
  `InvalidReply`. A device that cannot load the vocal model, or has no fusion model, answers `Unavailable` for those
  jobs only, for the life of the client; analysis goes on.
- `governor::verdict(Conditions) -> Verdict` and `rest_after(busy, duty_cycle)`: the power and temperature policy,
  pure and tested.
- Re-exports `Device`, `ANALYZER_VERSION`, `BEAT_MODEL_FILENAME` and `VOCAL_MODEL_FILENAME` from `yt-mir`, so the application depends on this
  crate alone; `EXIT_CONFIGURATION` (2) is the worker's exit status for a missing file or argument.

## The process

`ytu-worker --runtime <ONNX Runtime library> --beat-model <file> [--vocal-model <file>] [--fusion-program <file>
--fusion-models <dir>] --model-cache <dir> --idle-seconds <1–3600>`. A missing flag or file exits with
`EXIT_CONFIGURATION` before anything else; incomplete fusion files only disable fusions. A reader
thread forwards requests and applies cancellations; the main thread runs jobs one after another and exits after the
idle time without jobs or when stdin closes (the application went away), cancelling the running job. Every job
checks, in order: the origin's `SourceRights` (`yt-core`; measuring vocals keeps nothing derived from the audio, so
it is analysis; separating a window for playback needs `separate`, which the library and YouTube grant), for a separation
that the window is non-empty and at most `yt_media::MAX_STEM_SECONDS` and that the output is a new file in an existing
folder (`create_new`: it never replaces one), the governor, decoding (`yt_media::decode_file`, at most 15 minutes of audio), then loads its model
once and runs under the governor's pace. The beat model loads on the GPU first and on the CPU only when the GPU cannot
run it; the vocal model loads on the GPU only, since on the CPU a song would take minutes of full load. A load
failure, even a panic, is remembered and answered as `Unsupported`.

An AI fusion needs both origins' `generate` right (the library and YouTube grant it), a valid fusion, a speed
within `FUSION_SPEEDS`, a tempo within `TrackAnalysis::TEMPO_BPM` and a new output file. It decodes both songs, builds
the canvas and runs the model in its own process, `sa3-generate` (sa3.cpp, C++ on ggml with Metal; the one piece of
the DJ not written in Rust), which loads Stable Audio 3 Medium on the GPU, renders, and exits, so its memory (about
3.6 GB at peak) returns at once. The governor can stop it while it runs; the rest the burst owes the duty cycle is
taken before the next job, so the clip is not delayed.

## Power and temperature

| Conditions | Verdict |
|---|---|
| Mains power, nominal temperature | Run, busy at most 50 % of the time (`AC_DUTY_CYCLE`) |
| Battery, warm (`Fair`) or unknown power | Run, busy at most 25 % (`REDUCED_DUTY_CYCLE`) |
| Low Power Mode, battery saver, battery below about 20 % | `Deferred(LowPower)` |
| `Serious` or `Critical` temperature | `Deferred(Thermal)`, also in the middle of a job |

The worker re-reads the conditions at most once a second between pieces of work and sleeps after each piece so the
busy share holds (`yt_mir::Pace`). macOS reads `ProcessInfo.thermalState`, Low Power Mode, the providing power source
and the battery warning level (IOKit), and runs each job's thread at the *utility* QoS class on mains power and
*background* otherwise; CoreML's queues inherit it. Windows reads `GetSystemPowerStatus` (no public thermal state)
and puts the whole process in EcoQoS with a below-normal priority class. Each GPU piece is one model chunk of 30 s of
audio, far below the Windows GPU watchdog.

## Lifecycle and restarts

The client starts the worker with the first job and replaces it when it is within ten seconds (at most half the idle
time) of its idle exit, so a job never races the exit; a worker that still exits cleanly before reading a job gets
one replacement. Any other exit is a crash: the next start waits 2 s, doubling up to 60 s, and after five crashes in
an hour the worker stays down until the oldest leaves the hour. A worker that answers garbage, or does not answer a
cancellation within 5 s, is stopped. The worker's stderr is discarded because messages could name what the user plays;
on Windows it starts without a console window.

## Trust boundary

- The worker trusts nothing it reads: frames are bounded, versions checked, unknown actions answered with
  `Failed { Protocol }`, paths must be regular files, and the audio is validated by `yt_mir::Pcm`.
- The application treats the worker as untrusted too: it validates every reply and analysis, and stops a worker that
  breaks the protocol.
- Rights are checked in the worker as well as in the planner, so a job that breaks a source's `SourceRights` is
  refused at the place that would do the work.

## Performance (Apple M4 Pro on battery, 11 November 2026)

- First job: 87 s, almost all of it CoreML compiling the model at background QoS; the compiled model is kept in
  `--model-cache` and later starts load it in seconds.
- A 60-second track afterwards: 7.9 s of wall time at a 25 % duty cycle on efficiency cores, about 2 s of work.
- Vocals: about 3.7 s of GPU per minute of audio (0.41 s per 7.8 s segment), so a 4-minute song takes about a
  minute at a 25 % duty cycle; the vocal model adds 174 MB to the process while it runs.
- The process's memory (about 1 GB with the compiled model) is returned three minutes after the last job.
- A fusion (M4 Pro on mains, 12 November 2026): an 8-bar fusion at 101 BPM, a 38-second canvas, renders in 13 s
  including the model's load from a warm file cache; a 10-second canvas in 5–7 s. Peak memory of `sa3-generate`
  3.6 GB.

## Verification

- `cargo test -p yt-worker`: framing, versions, bounded details, governor verdicts and rest, restart waits, option
  parsing (the vocal model is optional but must exist), and the real process through its client and raw protocol
  (unreadable audio, a device that cannot load the runtime, vocals without a vocal model are unavailable while
  analysis goes on, separating needs the right, a new output and a bounded window, fusions need their model and the
  right to generate, idle exit and restart, missing worker or model, cancellation, protocol refusal, configuration
  exit), and the fusion canvas (both songs blended with equal power, room on both sides, bounds, the clip's seams and
  level, the prompt).
- `YTU_ONNXRUNTIME=<library> YTU_SA3_PROGRAM=<sa3-generate> YTU_SA3_MODELS=<folder> cargo test --release -p
  yt-worker -- --ignored` analyzes a drum pattern on the GPU and cancels a second analysis, measures the vocals of a
  drum pattern and separates a window of it, and renders a fusion of two drum patterns; they need the beat and vocal
  models in `crates/yt-mir/models` and the fusion kit (`docs/RUST_PACKAGING.md`).
- By hand, `cargo run --release -p yt-worker --example fusion_probe -- <outgoing> <incoming> <ONNX Runtime> <beat
  model> <sa3-generate> <models> <out>` renders one fusion of two files and writes `blend.wav` (no model) and
  `fusion.wav` (with it) for listening.
- `cargo xwin clippy --target x86_64-pc-windows-msvc -p yt-worker --all-targets -- -D warnings` checks the Windows
  platform code from macOS.
