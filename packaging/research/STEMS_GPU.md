# Portable BS-RoFormer graph experiment

This is optional model preparation tooling, not an application runtime dependency
or an installer step. It does not replace the shipped `bs-roformer-v1.pt`, change
its SHA-256, or modify the published v5.0.0 artifacts.

The pinned frozen graph contains 677 float32 tensor constants, eight scalar
float64 constants, four scalar int64 constants used in shape calculations, and
one fixed CPU device. It has no remaining parameters or buffers. PyTorch explains
that [`Module.to` cannot change a frozen graph's constants](https://github.com/pytorch/pytorch/blob/main/torch/jit/_freeze.py),
and [Metal does not support float64 tensors](https://docs.pytorch.org/docs/main/notes/mps.html).

`export-portable-stems.py` converts the eight float64 scalar tensors to float32,
turns the four integer shape constants into integer graph constants, and derives
the device from the input instead of hard-coding CPU. This avoids moving shape
scalars to Metal while their shape calculations remain on CPU. Model weights,
the architecture and the eight-second input contract are unchanged. The script
checks the original SHA-256, PyTorch version and exact transformation counts,
requires bit-identical CPU output on its reference window, and never overwrites
the source or an existing destination. It uses private TorchScript graph APIs
and is deliberately restricted to PyTorch 2.12.

Run in the existing development environment, with new output paths:

```sh
backend/venv/bin/python packaging/research/export-portable-stems.py \
  crates/yt-media/models/bs-roformer-v1.pt /tmp/bs-roformer-experimental.pt \
  --reference-wav /path/to/stereo-44100.wav --start 25 \
  --mps --report /tmp/bs-roformer-experimental.json
```

Without `--reference-wav`, it uses deterministic synthetic audio. `--mps` is
optional and requires actual Metal access. The report separates model loading
from inference and runs Metal twice. A candidate is approximately 639 MB.
This export command uses Python at build/research time; the application continues
to use Rust/LibTorch only.

## Observations on 2026-11-08

PyTorch 2.12 on the local Apple Silicon Mac with macOS 26.2 successfully executed
STFT, ISTFT and the complete candidate on Metal. For one eight-second window of
real musical audio:

| Measurement | Observed result |
| --- | --- |
| Candidate CPU versus published CPU | Bit-identical; max and RMS error zero |
| Original CPU inference | 6.65 s |
| Candidate CPU inference | 6.57 s |
| Candidate Metal loading | 5.32 s |
| Candidate Metal inference, first/repeated | 20.12 / 24.92 s |
| Metal versus original CPU maximum absolute error | 0.00045314 |
| Metal versus original CPU RMS error | 0.000032433 |
| Metal versus original CPU signal/error ratio | 75.24 dB |
| Invalid output samples | None |

A separate synthetic window was also CPU bit-identical; Metal maximum error was
1.18e-9. These comparisons establish this candidate's behavior for the tested
windows and hardware, not equivalence across all audio or supported systems.

The candidate is **not selected for the release**: Metal took roughly three to
four times the CPU inference time on this Mac. Native Rust inference, Windows,
and earlier supported macOS releases would also need candidate validation before
distribution. No fast-math, reduced-precision weights, global MPS fallback or
memory-limit overrides were enabled. The stable model and its existing safe CPU
fallback remain the release behavior.
