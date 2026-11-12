# yt-mir

Music analysis for the DJ: where a track's beats and bars fall, its meter, tempo, key, loudness and sections, and
how loud its vocals are, measured from its decoded samples and returned as a validated `yt_core::TrackAnalysis`. The design and its measurements live in
[`docs/AI_DJ.md`](../../docs/AI_DJ.md) (§4.1 and §8.1).

## API

- `init_runtime(&Path) -> Result<()>` loads the ONNX Runtime library once per process (`ort` with `load-dynamic`;
  the application bundles the library). Later calls return the first result.
- `BeatModel::load(path, Device, cache) -> Result<BeatModel>` checks the file against `BEAT_MODEL_SHA256` and opens
  it. `Device::Gpu` uses CoreML (macOS, MLProgram on the GPU) or DirectML (Windows) and fails instead of falling
  back to the CPU; `Device::Cpu` runs ONNX Runtime's CPU provider on one thread. `cache` holds CoreML's compiled
  model.
- `Pcm::new(samples, channels, sample_rate) -> Result<Pcm>` validates interleaved `f32` input: 1–8 channels,
  `TrackAnalysis::SAMPLE_RATE`, whole frames, finite values, between `Pcm::MIN_SECONDS` (5 s) and
  `TrackAnalysis::MAX_DURATION_SECONDS`.
- `Analyzer::new(BeatModel)` and `Analyzer::analyze(&Pcm, &dyn Pace) -> Result<TrackAnalysis>`: the whole
  pipeline. `Pace::proceed(busy)` is asked between steps, after every model chunk and every 65,536 samples of the key
  profile, with how long the last piece kept the device busy; `false` stops with `MirError::Cancelled`. An
  `AtomicBool` is a `Pace` that only cancels; `ytu-worker`'s governor also sleeps there to bound the duty cycle.
- `VocalModel::load(path, Device, cache)` checks the file against `VOCAL_MODEL_SHA256` (`VOCAL_MODEL_FILENAME`) and
  opens it like the beat model; `VocalModel::measure(&Pcm, &dyn Pace) -> Result<Vec<f32>>` returns the RMS level of
  the separated vocals every `TrackAnalysis::LOUDNESS_STEP_SECONDS`, in dBFS (`TrackAnalysis::vocal_db`), asking
  `pace` after every segment. A second, slower job than `analyze`, so the planner never waits for it.
- `Device` (`Gpu`, `Cpu`) serializes in snake case for the worker protocol.
- `MirError`: `InvalidAudio`, `Model`, `Runtime`, `Cancelled`, `Core` (a `TrackAnalysis` bound was broken).
- `ANALYZER_VERSION` is written into every analysis; raise it whenever any step changes its output so caches
  measure again.

## Pipeline

1. **Mono mix and resampling** (`audio.rs`): the channel average, resampled to 22,050 Hz with rubato's FFT
   resampler, delay-compensated.
2. **Log-mel spectrogram** (`spectrogram.rs`): Beat This!'s `LogMelSpect` reproduced exactly — centred STFT with
   reflect padding, periodic Hann window of 1,024 samples, hop 441 (50 frames per second), magnitudes scaled by
   `1/sqrt(1024)`, a Slaney mel filterbank of 128 bands from 30 Hz to 11 kHz without area normalization, then
   `ln(1 + 1000 · mel)`.
3. **Beat model** (`model.rs`): chunks of 1,500 frames with 6-frame borders, merged with "keep first", exactly as
   `beat_this.inference.split_predict_aggregate` (`beats.rs`).
4. **Peaks** (`beats.rs`): the reference "minimal" postprocessor — maxima within ±3 frames with a logit above 0,
   adjacent ones merged into their mean, downbeats moved to the nearest beat.
5. **Grid** (`grid.rs`): tempo from a least-squares line through the longest run of steady beats (precise despite
   the 20 ms frames), tempo stability, beats per bar from the downbeat spacing, and binary or ternary subdivision
   from the onset strength at each beat's middle and thirds.
6. **Key**: `yt_media::Analyzer::key`, the same pitch-class profile and Krumhansl–Kessler correlation the library's
   audio features use, so both agree on what a key is.
7. **Loudness** (`loudness.rs`): EBU R128 integrated, short-term (3 s) and momentary (400 ms) loudness every half
   second and the true peak (`ebur128`), and the low/mid/high band levels of every bar. The momentary curve tells
   precisely where the sound starts and the music ends (`TrackAnalysis::audible_start`, `audible_end`).
8. **Sections** (`structure.rs`): Foote's novelty over bar-level self-similarity of loudness and timbre (the DCT of
   each bar's mean log-mel spectrum), with a kernel of four bars on each side, as Vande Veire and De Bie's automatic
   DJ does; boundaries are novelty peaks of at least 30 % of the strongest, moved onto the four-bar phrase grid whose
   lines carry the most novelty, at least four bars apart. A section is strong when its level is above the song's
   average.

### Vocals

`VocalModel::measure` (`separation.rs`) runs HTDemucs v4 on stereo at 44.1 kHz: segments of 343,980 samples (39/5 s,
the length it was trained on) every three quarters of one, blended with triangular weights, and short segments
centred between the song's neighbouring samples, exactly as `demucs.apply.apply_model`. Around the network,
`stft.rs` reproduces `HTDemucs._spec` and `_ispec` (4,096-point periodic Hann window, hop 1,024, unitary scaling,
the extra reflect padding, the Nyquist bin and two edge frames dropped). The vocals are the waveform branch plus the
inverse transform of the spectrogram branch; their level is accumulated as samples settle, so only one segment and
a quarter of audio is held, and the separated audio is dropped as it is measured.

## Invariants and trust boundary

- Input samples are validated by `Pcm::new`; the result is checked by `TrackAnalysis::validate` before it is
  returned. Beat positions are frames at the source rate, strictly increasing and inside the track.
- Only exported models whose SHA-256 is `BEAT_MODEL_SHA256` or `VOCAL_MODEL_SHA256` load; nothing executable is
  read at run time.
- The separated vocals never leave `VocalModel::measure`: only their levels do, so measuring them is analysis under
  `SourceRights`, not separation for playback.
- `Device::Gpu` never silently runs on the CPU (`error_on_failure`). The exported graph runs entirely in CoreML (one
  partition of 982 nodes), so no part of it falls back to the CPU either.
- Deterministic for equal input on the same device.

## Models

`models/export_beat_this.py` exports the authors' `final0` checkpoint (MIT) and rewrites, with identical math, the
operations CoreML would leave on the CPU. `models/export_htdemucs.py` exports HTDemucs v4 without its STFT and ISTFT;
its weights have no declared license, so it is not bundled until the owner's review (`docs/AI_DJ.md` §7.4). See
[`models/README.md`](models/README.md). Neither the 81 MB `models/beat-this-final0.onnx` nor the 174 MB
`models/htdemucs-core.onnx` is published with the sources (the publish script blocks `.onnx`).

## Performance (Apple M4 Pro, 11 November 2026)

- A 45-second mix: 0.30 s of analysis on the GPU, CPU below 0.1 s for the network. A 3:30 track takes about 0.7 s
  of GPU.
- Loading the model: 13.5 s the first time (CoreML compiles it), 3.4 s with the compiled model in `cache` (306 MB).
  Peak resident memory 1.1 GB with the cache, 2.0 GB while compiling.
- On three Apple Loops mixes with known beats, beat F1 1.0 and downbeat F1 0.98–1.0, tempo within 0.03 BPM.
- Vocals: 0.41 s of GPU per 7.8 s segment; a 4:27 song in 16.4 s at full speed, about a minute under the worker's
  25 % duty cycle. Loading: 29 s the first time (CoreML compiles it), 7 s from the cache. The levels match demucs'
  `apply_model` within 0.004 dB (CPU) and 0.000 dB (GPU).

## Limits

- Ternary subdivision is a heuristic; it is validated on corridos tumbados, reggaeton and cumbia in stage 1 of the
  plan before the planner relies on it.
- Downbeats inherit the model's errors on unusual meters; the planner treats `Meter::confidence` as a weight.
- Sections are strong or calm, not named (intro, verse, chorus): LinkSeg's license is pending.
- The vocal model needs a GPU; the worker never runs it on the CPU, where a song would take minutes of full load.

## Verification

- `cargo test -p yt-mir`: spectrogram (reflect padding, Slaney scale, frame count, tone placement), chunking and
  aggregation against the reference formula, peak picking, downbeat snapping, grid (tempo fit, meter, ternary
  subdivision, rubato), loudness and band energy, sections (an intro, chorus and outro on their bars; boundaries off
  the phrase grid; short and uniform songs), PCM validation, resampling, model hash refusal, the STFT (the inverse
  returns the signal away from segment edges; a sine lands in its bin with unitary scaling; reflection) and the
  segment blending (overlapping segments blend back to their signal, holding one segment; demucs' weights).
- `cargo run --release -p yt-mir --example beat_probe -- <audio> <libonnxruntime> models/beat-this-final0.onnx
  [--cpu] [--cache <dir>] [--beats-json <file>]` analyzes a file with the real model. During development the
  library can be the one inside a Python `onnxruntime` 1.24 or later (`onnxruntime/capi/libonnxruntime.*.dylib`).
- `cargo run --release -p yt-mir --example vocal_probe -- <audio> <libonnxruntime> models/htdemucs-core.onnx
  [--cpu] [--cache <dir>] [--reference <vocals.f32>]` prints a file's vocal levels, or, with `--reference`, compares
  them with the vocals `export_htdemucs.py --probe` wrote.
