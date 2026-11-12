# Models

## Beat model

`beat-this-final0.onnx` is the `final0` checkpoint of Beat This! (Foscarin, Schlüter and Widmer, ISMIR 2024; code and
weights MIT, <https://github.com/CPJKU/beat_this>) exported for ONNX Runtime. It is ignored by version control and
blocked by the publish script; a distribution supplies it next to the application.

| Property | Value |
| --- | --- |
| Source checkpoint | `final0.ckpt`, 81,058,141 bytes, from `https://cloud.cp.jku.at/public.php/dav/files/7ik4RrBKTS273gp/final0.ckpt` |
| Input `spect` | float32 `[1, 1500, 128]`: 1,500 log-mel frames (30 s at 50 frames per second) |
| Outputs `beat`, `downbeat` | float32 `[1, 1500]` logits |
| Size | 81,367,511 bytes |
| SHA-256 | `65d122c994112307303403c3f3e40e3d6f271cb2d7b6790508c1b48d3655b942` (`yt_mir::BEAT_MODEL_SHA256`) |
| Exported with | PyTorch 2.12.0, ONNX opset 17, checked with ONNX Runtime 1.26.0 |

### Export

```sh
pip install --no-deps beat_this==1.1.0
python export_beat_this.py final0.ckpt beat-this-final0.onnx
```

The script loads the checkpoint with `weights_only=True`, exports the untouched graph as a reference and then a
graph whose operations CoreML can all run on the GPU. The rewrites use identical math:

| Stock operation | Rewrite |
| --- | --- |
| Rotary embedding with empty left and right slices | The full rotation without slicing, `Neg` written as `× −1` |
| `RMSNorm` through `F.normalize` (`ReduceL2`, `Expand`) | `x / max(√(Σx²), 1e-12)` with broadcasting |
| `scaled_dot_product_attention` | `softmax(q·kᵀ/√d)·v` written out |
| `BatchNorm1d` in inference | Its fixed per-channel scale and shift |

It refuses to write the result when the two graphs differ by more than `1e-3` on the CPU provider (the measured
difference is 1.7e-6) and prints the SHA-256. Two runs produce the same bytes.

### Measured effect

On the Apple M4 Pro reference machine, with the ONNX Runtime CoreML provider on the GPU: the stock export put 929 of
1,076 nodes in CoreML across 76 partitions and spent 6.5–12 s of CPU per 3:30 track; this export puts all 982 nodes
in one partition, takes 0.72 s per track and 0.05 s of CPU, with identical beat and downbeat F1 and logits within
8e-5 of the stock graph. See `docs/AI_DJ.md` §8.1.

## Vocal model

`htdemucs-core.onnx` is HTDemucs v4 (Rouard, Massa and Défossez, ICASSP 2023; code MIT,
<https://github.com/facebookresearch/demucs>), the `htdemucs` (955717e8) checkpoint, exported without its STFT and
ISTFT, which yt-mir computes with identical math (`src/stft.rs`). It is ignored by version control and blocked by
the publish script. **Its weights carry no declared license** (trained on MUSDB18-HQ plus 800 songs; the repository
is archived): it is not bundled with any package until the owner's legal review (`docs/AI_DJ.md` §7.4). Without it the
DJ measures no vocals and judges them by the mid band alone.

| Property | Value |
| --- | --- |
| Source weights | `955717e8.safetensors`, 84,025,440 bytes, the author's safetensors copy at `https://huggingface.co/adefossez/HTDemucs` (the original `dl.fbaipublicfiles.com` pickle has an expired certificate and holds code) |
| Input `mix` | float32 `[1, 2, 343980]`: 39/5 s of stereo at 44.1 kHz |
| Input `spec` | float32 `[1, 4, 2048, 336]`: the mix's spectrogram, real and imaginary parts as channels |
| Outputs `spec_out`, `wave_out` | float32 `[1, 4, 4, 2048, 336]` and `[1, 4, 2, 343980]`: both branches for drums, bass, other and vocals |
| Size | 174,264,755 bytes |
| SHA-256 | `6dc871812d60d6c3581e317c83231ac5c89a15aff7fd2506b3c7210aff5c6117` (`yt_mir::VOCAL_MODEL_SHA256`) |
| Exported with | PyTorch 2.15.0, demucs 4.0.1, ONNX opset 17, checked with ONNX Runtime 1.31.1 |

### Export

```sh
pip install --no-deps demucs==4.0.1 dora-search treetable submitit retrying
python export_htdemucs.py 955717e8.safetensors htdemucs-core.onnx --probe probe
```

The script refuses to write the result unless the wrapper rebuilds the full model's output exactly (measured
difference 0) and ONNX Runtime matches PyTorch within `1e-3` (measured 1.75e-4); two runs produce the same bytes.
`--probe` writes one segment and the vocals of demucs' own `apply_model` for `examples/vocal_probe.rs`.

### Measured effect

On the M4 Pro with the CoreML provider on the GPU, 1,426 of 1,451 nodes run in CoreML: 0.41 s per 7.8 s segment
after the first run, 16 s of busy GPU for a 4:27 song at full speed (about a minute under the worker's 25 % duty
cycle). The first load compiles the model (29 s, once; 7 s from CoreML's cache after that). The vocal levels match
demucs' `apply_model` within 0.004 dB on the CPU and 0.000 dB on the GPU (`vocal_probe --reference`).
