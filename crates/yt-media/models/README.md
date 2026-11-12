# Native vocal separation model

## Metal edition for Apple Silicon

This local edition uses the existing `model_bs_roformer_ep_317_sdr_12.9755.ckpt` vocal checkpoint by viperx. The model takes stereo audio at 44.1 kHz in eight-second windows with 75% overlap. Rust/Candle expands the stored float16 weights to float32 and runs the neural network on Metal with fused attention, rotary embeddings and normalization. Rust also handles the Fourier transforms, overlap, and output. If Metal is unavailable or inference fails, the operation reports an error before publishing the stems.

Convert the original checkpoint with the Rust tool. The tool verifies both the source and output SHA-256 and refuses to overwrite an existing file.

```sh
cargo run --locked --release -p yt-media --features metal-stems \
  --example convert_stems_model -- \
  backend/models/audio-separator/model_bs_roformer_ep_317_sdr_12.9755.ckpt \
  crates/yt-media/models/bs-roformer-viperx-f16.safetensors
```

The 305 MiB export has SHA-256 `97307b43fa9a830a80e7d382fab39a746b2a0777ba38cf4d1c7432ef77ad19a2`. It is ignored by version control.

On the development Mac, `stems_probe` processed 18 seconds of stereo audio in 31.8 seconds with Metal and 52.9 seconds with the previous CPU build. Across eight [MUSDB18 excerpts](https://sigsep.github.io/datasets/musdb.html), the largest absolute SDR change for either stem was 0.0034 dB. These measurements describe the tested hardware and excerpts.

## TorchScript edition

`bs-roformer-v1.pt` is an exported TorchScript inference graph of the existing
`model_bs_roformer_ep_317_sdr_12.9755.ckpt` model. The large binary is deliberately
ignored by version control and must be supplied to a native-stems distribution.

| Property | Value |
| --- | --- |
| Input | float32 `[1, 2, 352800]`, stereo, 44,100 Hz |
| Output | float32 `[1, 2, 352800]`, vocals |
| Window | 8 seconds; shorter input is padded with silence |
| Runtime | LibTorch 2.12; detected CUDA/MPS with safe CPU fallback |
| Export SHA-256 | `31d794ab054bd70964b6165ceca3b94e00f344f8a0b8bc325def47ce9aa14926` |

The Rust separator performs overlapping inference, writes vocals to a float WAV
and derives the instrumental as the original waveform minus the estimated vocal.
This is a trained neural model, not channel subtraction masquerading as vocals.
The residual construction preserves reconstruction of the source. Memory is
bounded by a few audio windows and the model's inference memory.

## Building and running

Set `LIBTORCH` to a standalone LibTorch 2.12 distribution containing `include/`
and `lib/`, then build with `--features native-stems`. The `tch` commit is pinned
to the upstream revision supporting that exact native ABI. Development on this
machine uses the native files already installed under
`backend/venv/lib/python3.14/site-packages/torch` as `LIBTORCH`; this does not embed
or launch that environment's interpreter in the application.

For the synthetic inference check:

```sh
LIBTORCH=/path/to/libtorch DYLD_LIBRARY_PATH=/path/to/libtorch/lib \
  cargo run -p yt-media --features native-stems --example stems_probe -- \
  crates/yt-media/models/bs-roformer-v1.pt 9
```

The optional final number selects 1–60 seconds. Nine seconds exercises overlapping
windows; a single second tests zero padding. The probe checks exact output frame
counts, finite and nonzero estimates, waveform reconstruction and cancellation
cleanup. It creates only temporary synthetic files and does not need audio output.

macOS packaging must include `libtorch.dylib`, `libtorch_cpu.dylib`, `libc10.dylib`
and `libomp.dylib` and make them discoverable through the application's rpath.
`libtorch_python.dylib` and the Python interpreter are not needed. The local
arm64 LibTorch binaries require macOS 14.0 or newer. Windows builds need the
matching Windows LibTorch distribution and its native DLLs beside the executable.
The same TorchScript model is portable across these CPU platforms; a Windows
runtime test is still required on a Windows machine.

## Export provenance

The original checkpoint and YAML configuration were loaded once by the existing
development environment's `audio_separator` `BSRoformer` implementation, using
PyTorch 2.12.0. The model was placed in evaluation mode, all parameters disabled
for gradient computation, and traced with a zero tensor of the input shape above.
`torch.jit.trace(..., check_trace=False, strict=False)` followed by
`torch.jit.freeze(traced.eval())` produced the portable graph. Both the exporter
and the Rust probe validated the resulting output. This export step belongs to
model preparation; the shipped app loads the graph directly through Rust/LibTorch.

The graph is fixed to the stated window size. Do not substitute another model
or trace shape under this filename. `StemSeparator` validates output shape and
rejects non-finite model output instead of publishing corrupted stems.

## Current limits

Only vocals/instrumental separation is provided by this model. Multi-instrument
stems are not provided. The loader detects CUDA and Apple Metal when the runtime
reports availability and retries on CPU if loading or inference fails. The
actual device and a bounded fallback reason are exposed by `StemSeparator` and
`StemReceipt`. No partial output is published on an accelerator error; the same
input window is retried on CPU. Windows Full currently bundles CPU LibTorch and
therefore cannot use CUDA with that distribution.

On this Mac, a real Metal-enabled probe detected MPS but the frozen graph's
float64 constants prevented loading on MPS. CPU fallback then completed 44,100
stereo frames in 7.09 seconds with exact reconstruction and cancellation cleanup.
Thus detection/fallback is verified; accelerated inference of this artifact is
not established. The CALIDAD DSP
chain is ported separately in [`yt-quality`](../../yt-quality/README.md), which
also provides `prepare_source` using SoXR HQ before model inference. Direct
callers that skip preparation retain the separator's Rodio linear conversion;
44.1 kHz input is unchanged.
The output is always stereo 44.1 kHz float WAV. Cancellation takes effect between
native inference calls and while decoding/writing; an active LibTorch call runs
to completion before cancellation is observed.

An optional [portable-graph research exporter](../../../packaging/research/STEMS_GPU.md)
demonstrates a separate float32 candidate with identical CPU output and working
Metal inference. On the tested Mac its Metal inference was slower than CPU, so
the shipped model, its SHA-256 and the safe CPU fallback remain unchanged.
