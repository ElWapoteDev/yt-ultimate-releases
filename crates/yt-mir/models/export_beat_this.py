"""Export Beat This! (CPJKU, MIT) to ONNX so CoreML and DirectML can run the whole graph on the GPU.

Model preparation only: the application never runs Python. Usage:

    python export_beat_this.py final0.ckpt beat-this-final0.onnx

Needs torch, onnx, onnxruntime, einops, rotary-embedding-torch and the beat_this 1.1.0 sources
(`pip install --no-deps beat_this==1.1.0`). The checkpoint is the authors' `final0.ckpt`
(https://cloud.cp.jku.at/public.php/dav/files/7ik4RrBKTS273gp/final0.ckpt, 81,058,141 bytes).

ONNX Runtime's CoreML provider leaves some operations of the stock export on the CPU (zero-size slices
in the rotary embedding, Neg, ReduceL2, the BatchNorm1d of the stem). This script replaces them with
the same math written with supported operations, exports the untouched graph too, and refuses to write
the result when the two differ by more than MAX_DIFF on the CPU provider. It prints the SHA-256 that
yt-mir checks before loading the file.
"""

import hashlib
import inspect
import pathlib
import sys
import tempfile

import numpy as np
import onnxruntime as ort
import rotary_embedding_torch.rotary_embedding_torch as rot
import torch
from einops import rearrange

from beat_this.model import roformer
from beat_this.model.beat_tracker import BeatThis
from beat_this.utils import replace_state_dict_key

CHUNK_FRAMES = 1500
MEL_BANDS = 128
OPSET = 17
MAX_DIFF = 1e-3


class Exportable(torch.nn.Module):
    """BeatThis returns a dict; ONNX outputs are positional."""

    def __init__(self, model):
        super().__init__()
        self.model = model

    def forward(self, spect):
        out = self.model(spect)
        return out["beat"], out["downbeat"]


class Affine1d(torch.nn.Module):
    """BatchNorm1d in inference: a fixed per-channel scale and shift."""

    def __init__(self, bn):
        super().__init__()
        scale = bn.weight / torch.sqrt(bn.running_var + bn.eps)
        self.register_buffer("scale", scale.detach()[:, None])
        self.register_buffer("shift", (bn.bias - bn.running_mean * scale).detach()[:, None])

    def forward(self, x):
        return x * self.scale + self.shift


def rotate_half(x):
    x = rearrange(x, "... (d r) -> ... d r", r=2)
    x1, x2 = x.unbind(dim=-1)
    return rearrange(torch.stack((x2 * -1.0, x1), dim=-1), "... d r -> ... (d r)")


def apply_rotary_emb(freqs, t, start_index=0, scale=1.0, seq_dim=-2):
    if t.ndim == 3:
        freqs = freqs[-t.shape[seq_dim]:]
    if start_index != 0 or freqs.shape[-1] != t.shape[-1]:
        raise ValueError("Beat This! rotates every feature; partial rotation is not expected")
    return (t * freqs.cos() * scale) + (rotate_half(t) * freqs.sin() * scale)


def rms_norm(self, x):
    norm = torch.sqrt((x * x).sum(dim=self.dim, keepdim=True))
    return x / norm.clamp_min(1e-12) * self.scale * self.gamma


def attend(self, q, k, v):
    if self.scale is not None:
        q = q * (self.scale / q.shape[-1] ** -0.5)
    weights = torch.matmul(q, k.transpose(-1, -2)) * (q.shape[-1] ** -0.5)
    return torch.matmul(weights.softmax(dim=-1), v)


def load(path):
    checkpoint = torch.load(path, map_location="cpu", weights_only=True)
    accepted = set(inspect.signature(BeatThis).parameters)
    model = BeatThis(**{k: v for k, v in checkpoint["hyper_parameters"].items() if k in accepted})
    model.load_state_dict(replace_state_dict_key(checkpoint["state_dict"], "model.", ""))
    return model.eval()


def export(model, path):
    torch.onnx.export(
        Exportable(model),
        (torch.zeros(1, CHUNK_FRAMES, MEL_BANDS),),
        str(path),
        input_names=["spect"],
        output_names=["beat", "downbeat"],
        opset_version=OPSET,
        dynamo=False,
    )


def main():
    checkpoint, output = (pathlib.Path(arg) for arg in sys.argv[1:3])
    if output.exists():
        raise SystemExit(f"{output} already exists")
    with tempfile.TemporaryDirectory() as scratch:
        reference = pathlib.Path(scratch) / "reference.onnx"
        export(load(checkpoint), reference)
        model = load(checkpoint)
        rot.apply_rotary_emb = apply_rotary_emb
        roformer.RMSNorm.forward = rms_norm
        roformer.Attend.forward = attend
        model.frontend.stem.bn1d = Affine1d(model.frontend.stem.bn1d)
        export(model, output)
        spect = np.abs(np.random.default_rng(0).standard_normal((1, CHUNK_FRAMES, MEL_BANDS), dtype=np.float32)) * 4
        expected = ort.InferenceSession(str(reference), providers=["CPUExecutionProvider"]).run(None, {"spect": spect})
        actual = ort.InferenceSession(str(output), providers=["CPUExecutionProvider"]).run(None, {"spect": spect})
    difference = max(float(np.abs(a - b).max()) for a, b in zip(expected, actual))
    if difference > MAX_DIFF:
        output.unlink()
        raise SystemExit(f"rewritten graph differs by {difference:.2e}; nothing written")
    digest = hashlib.sha256(output.read_bytes()).hexdigest()
    print(f"max difference {difference:.2e}; {output.stat().st_size} bytes; sha256 {digest}")


if __name__ == "__main__":
    main()
