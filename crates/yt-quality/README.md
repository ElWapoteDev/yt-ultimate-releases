# Native quality processing

This crate ports the DSP stages of the legacy CALIDAD path. Application logic is
Rust; SoXR is linked as a native library. No Python, NumPy, SciPy or FFmpeg process
is needed at runtime. The trained vocal model is provided separately by
`yt-media`'s `native-stems` feature.

## Pipeline and contracts

1. `prepare_source(input, output, &cancel)` decodes supported local formats and
   writes stereo 44.1 kHz float32 WAV. SoXR HQ performs anti-aliased rate conversion.
   Native 44.1 kHz audio is not resampled. Mono is duplicated; conventional
   multichannel layouts use a normalized center/surround matrix with LFE omitted.
2. The caller uses `yt_media::StemSeparator` to produce synchronized vocal and
   instrumental WAVs. Separation itself is not simulated by this crate.
3. `process_stems(vocals, instrumental, reference_wav, seed, &cancel, callback)`
   runs the stages below and returns an auditable `QualityReport`.
4. The caller encodes that reference as FLAC24 with `yt-audio` into a private
   temporary directory, then calls `verify_final_flac(reference, flac, &cancel)`.
   Only a verified candidate should be published to the library.

| Stage | Implementation |
| --- | --- |
| Instrumental EQ | f64 FFT2048/hop512, square-root periodic Hann analysis/synthesis, normalized overlap-add. Seven log-Gaussian bands at 100, 220, 500, 1100, 2500, 5500 and 11000 Hz. SHA256-derived phases and 0.14–0.34 Hz modulation; shared stereo envelope clamped to ±6 dB. Preserves the exact frame count, including short files and edge samples. |
| Vocal-preserving mix | Two passes select one instrumental gain in [0.05, 1] to prevent sample clipping. The separated vocal is unchanged. Saturated vocals or a required gain below 0.05 are rejected. |
| Initial PCM24 audit | Quantizes the mixed reference to 24-bit PCM precision and verifies `mix − gain × instrumental − vocal` against one PCM24 step. This reproduces the precision boundary of the legacy pre-remap FLAC intermediate without redundant encoding. |
| Shared remapping | One SoXR HQ variable-rate instance processes both audio channels plus 1 Hz sine/cosine audit markers. Base pitch +0.28 semitones, modulation 0.05/0.03 semitones with 13/23-second periods and 3-second edge tapers. Blocks are 441 frames, padded by 1024 frames at both ends. |
| Mapping audit | Marker phase verifies monotonic time, continuity, first/last source coverage and measured pitch below half a semitone. One mapping acts on the whole mix. |
| Peak estimation | Bounded overlapping blocks with an 8×, 161-tap Kaiser(beta=5) polyphase FIR equivalent to SciPy `resample_poly(x, 8, 1)` defaults. This is an engineering estimator, not an IEC certification. |
| Shared headroom | One constant gain targets −1.1 dBTP. No limiter, compressor or vocal-only gain is added. |
| Final audit | Checks actual FLAC STREAMINFO for stereo/sample rate/frame count and 24-bit depth, decodes the complete file, compares against the reference within 2^-23 and remeasures estimated peak against the −1 dBTP maximum. |

All DSP intermediates retain float64 precision except the explicitly requested
model input and PCM24 precision boundary. Processing reads bounded blocks; it
does not accumulate a complete song. Final FLAC encoding has the separate memory
limit documented by `yt-audio`. RIFF intermediates currently have a 4 GiB limit.
All stages support cancellation and use temporary files with no-clobber publication.

The conventional multichannel matrix is explicit in `prepare.rs`; unusual custom
channel layouts are not asserted to match FFmpeg's automatic matrix. This port
does not promise bit-identical neural separation or universal identification
avoidance. Its DSP algorithms and measurable bounds are tested against the prior
implementation.

## Verification

`cargo test -p yt-quality --lib` includes reference comparisons against the
original NumPy/SciPy/Python-SoXR implementation. Fixtures are synthetic and the
test suite does not invoke Python or access the network. It also verifies short
files, stereo relationships, anti-aliasing, clipping/headroom, cancellation,
existing-file protection, WAV_EXTENSIBLE stems and final FLAC audit rejection.

For the complete real-model path, configure LibTorch as described in
[`yt-media/models`](../yt-media/models/README.md) and run:

```sh
cargo run -p yt-quality --features native-stems --example quality_probe
```

This probe prepares synthetic input, runs BS-RoFormer, processes the stems,
exports a real FLAC24 and audits its decoded PCM and estimated peak. All outputs
live in an automatically removed temporary directory.

The complete probe passed on this Mac on 2026-11-08: 44,237 input frames became
43,526 output frames, maximum final PCM error was 8.94e-8 (below the 1.192e-7
limit), and the final estimated peak was −1.09999997 dBTP. Preparation, inference,
DSP, encoding and verification took 8.47 seconds. All ten unit/reference tests
and Clippy with warnings denied also passed. These measurements establish this
Mac execution; Windows native-model execution still requires its own validation.

## Native dependency

`soxr-sys` 0.1.3 bundles the SoX Resampler C sources and builds them through `cc`;
no separate SoXR installation is required. The Rust wrapper's Apache metadata
does not replace the bundled C source's LGPL-2.1-or-later terms. Preserve
[`LICENSE-SOXR`](LICENSE-SOXR) with distribution notices. Static-library source
and relinking material must be considered by the distribution packaging.

The native variable-rate implementation initializes process-global coefficient
tables without synchronization. The Rust wrapper serializes only creation of a
resampler; processing remains concurrent. A fresh-process regression starts 16
constructors simultaneously and compares every output sample and audit marker
with a serial reference. It reproduced a 0.414965 maximum sample error before
the guard and produces identical results with it. Signal audits and numerical
tolerances are unchanged.

Primary implementation references: [SoXR API](https://github.com/chirlu/soxr/blob/master/src/soxr.h),
[LiveKit Rust SDK sources](https://github.com/livekit/rust-sdks),
[RustFFT](https://github.com/ejmahler/RustFFT). The legacy contract is in
`backend/ytultimate/services/quality_audio.py` and `processing.py`.
