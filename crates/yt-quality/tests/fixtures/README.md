# Synthetic DSP parity fixtures

These files were generated locally on 2026-11-08 from the existing implementation
in `backend/ytultimate/services/quality_audio.py`; they contain no third-party
music or recordings. The tests consume the files directly and need no Python.

`input.wav`: 44,237 stereo float64 frames at 44.1 kHz. The first channel is
`0.7*sin(2*pi*440*t) + 0.06*sin(2*pi*11025*t + 0.25)` and the second is −0.5 times
the first, except explicit first/last transients `[0.31, -0.18]` / `[-0.23, 0.14]`.

`eq.wav`: original `dynamic_instrumental_eq`, seed `rust-parity-v1`, max gain 6 dB.
`remap.wav`: original `shared_smooth_remap` using Python-SoXR 1.1.0.
`reference.json`: complete original processing reports and SciPy true-peak result.

Rust reference tolerances: EQ 2e-14 absolute, SoXR output 2e-6 absolute with exact
output frame count, and FIR true-peak estimate 1e-12 absolute. These compare actual
sample arrays; they do not simply assert that reports describe their own output.
