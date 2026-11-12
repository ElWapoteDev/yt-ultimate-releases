"""Export HTDemucs v4 (Meta, code MIT) to ONNX without its STFT and ISTFT, which ONNX cannot express with
complex tensors; yt-mir computes both with identical math (src/stft.rs).

Model preparation only: the application never runs Python. Usage:

    python export_htdemucs.py 955717e8.safetensors htdemucs-core.onnx [--probe <prefix>]

Needs torch, onnx, onnxruntime, einops, safetensors and the demucs 4.0.1 sources
(`pip install --no-deps demucs==4.0.1` plus dora-search, treetable, submitit, retrying). The weights are
the `htdemucs` (955717e8) checkpoint republished by its author as safetensors, with the model's arguments
in the metadata (https://huggingface.co/adefossez/HTDemucs, 84,025,440 bytes); safetensors holds no code,
unlike the original pickle.

The exported graph takes the mix (`mix`, float32 [1, 2, 343980]: 39/5 s at 44.1 kHz) and its spectrogram
with real and imaginary parts as channels (`spec`, float32 [1, 4, 2048, 336]), and returns the denormalized
spectrogram branch (`spec_out`, [1, 4, 4, 2048, 336]) and waveform branch (`wave_out`, [1, 4, 2, 343980])
for drums, bass, other and vocals. A source is `wave_out` plus the inverse transform of `spec_out`. The
script refuses to write the result unless rebuilding the full model's output from the wrapper is exact
and ONNX Runtime matches PyTorch within MAX_DIFF; it prints the SHA-256 yt-mir checks. With `--probe` it
also writes one segment and the reference vocals of demucs' own `apply_model` for `vocal_probe`.
"""

import fractions
import hashlib
import json
import pathlib
import sys
import tempfile

import numpy as np
import onnxruntime as ort
import torch
from demucs.apply import apply_model
from demucs.htdemucs import HTDemucs
from einops import rearrange
from safetensors import safe_open
from safetensors.torch import load_file

MAX_DIFF = 1e-3
OPSET = 17


def load(path):
    with safe_open(path, "pt") as handle:
        metadata = handle.metadata()
    kwargs = json.loads(metadata["kwargs"])
    segment = kwargs.get("segment")
    if isinstance(segment, dict) and segment.get("_type") == "fraction":
        kwargs["segment"] = fractions.Fraction(segment["numerator"], segment["denominator"])
    model = HTDemucs(**kwargs)
    model.load_state_dict(load_file(path))
    return model.eval()


class Core(torch.nn.Module):
    """HTDemucs.forward between `_magnitude` and `_mask`, unchanged."""

    def __init__(self, model):
        super().__init__()
        self.m = model

    def forward(self, mix, mag):
        m = self.m
        x = mag
        B, C, Fq, T = x.shape
        mean = x.mean(dim=(1, 2, 3), keepdim=True)
        std = x.std(dim=(1, 2, 3), keepdim=True)
        x = (x - mean) / (1e-5 + std)
        xt = mix
        meant = xt.mean(dim=(1, 2), keepdim=True)
        stdt = xt.std(dim=(1, 2), keepdim=True)
        xt = (xt - meant) / (1e-5 + stdt)
        saved, saved_t, lengths, lengths_t = [], [], [], []
        for idx, encode in enumerate(m.encoder):
            lengths.append(x.shape[-1])
            inject = None
            if idx < len(m.tencoder):
                lengths_t.append(xt.shape[-1])
                tenc = m.tencoder[idx]
                xt = tenc(xt)
                if not tenc.empty:
                    saved_t.append(xt)
                else:
                    inject = xt
            x = encode(x, inject)
            if idx == 0 and m.freq_emb is not None:
                frs = torch.arange(x.shape[-2], device=x.device)
                emb = m.freq_emb(frs).t()[None, :, :, None].expand_as(x)
                x = x + m.freq_emb_scale * emb
            saved.append(x)
        if m.crosstransformer:
            if m.bottom_channels:
                b, c, f, t = x.shape
                x = rearrange(x, "b c f t-> b c (f t)")
                x = m.channel_upsampler(x)
                x = rearrange(x, "b c (f t)-> b c f t", f=f)
                xt = m.channel_upsampler_t(xt)
            x, xt = m.crosstransformer(x, xt)
            if m.bottom_channels:
                x = rearrange(x, "b c f t-> b c (f t)")
                x = m.channel_downsampler(x)
                x = rearrange(x, "b c (f t)-> b c f t", f=f)
                xt = m.channel_downsampler_t(xt)
        for idx, decode in enumerate(m.decoder):
            skip = saved.pop(-1)
            x, pre = decode(x, skip, lengths.pop(-1))
            offset = m.depth - len(m.tdecoder)
            if idx >= offset:
                tdec = m.tdecoder[idx - offset]
                length_t = lengths_t.pop(-1)
                if tdec.empty:
                    pre = pre[:, :, 0]
                    xt, _ = tdec(pre, None, length_t)
                else:
                    skip = saved_t.pop(-1)
                    xt, _ = tdec(xt, skip, length_t)
        S = len(m.sources)
        x = x.view(B, S, -1, Fq, T)
        x = x * std[:, None] + mean[:, None]
        xt = xt.view(B, S, -1, mix.shape[-1])
        xt = xt * stdt[:, None] + meant[:, None]
        return x, xt


def main():
    if len(sys.argv) not in (3, 5) or (len(sys.argv) == 5 and sys.argv[3] != "--probe"):
        sys.exit(__doc__)
    model = load(sys.argv[1])
    output = pathlib.Path(sys.argv[2])
    segment = int(model.segment * model.samplerate)
    torch.manual_seed(0)
    mix = torch.randn(1, 2, segment) * 0.1
    core = Core(model).eval()
    with torch.no_grad():
        reference = model(mix)
        z = model._spec(mix)
        mag = model._magnitude(z)
        spec_out, wave_out = core(mix, mag)
        rebuilt = model._ispec(model._mask(z, spec_out), segment) + wave_out
    exact = float((rebuilt - reference).abs().max())
    if exact != 0.0:
        sys.exit(f"the wrapper does not rebuild the model's output (difference {exact})")
    with tempfile.TemporaryDirectory() as folder:
        path = pathlib.Path(folder) / "core.onnx"
        torch.onnx.export(core, (mix, mag), path, input_names=["mix", "spec"],
                          output_names=["spec_out", "wave_out"], opset_version=OPSET, dynamo=False)
        session = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])
        onnx_spec, onnx_wave = session.run(None, {"mix": mix.numpy(), "spec": mag.numpy()})
        diff = max(float(np.abs(onnx_spec - spec_out.numpy()).max()),
                   float(np.abs(onnx_wave - wave_out.numpy()).max()))
        if diff > MAX_DIFF:
            sys.exit(f"ONNX Runtime differs from PyTorch by {diff}")
        data = path.read_bytes()
    output.write_bytes(data)
    print(f"wrote {output} ({len(data)} bytes); ONNX vs PyTorch {diff:.2e}")
    print("SHA-256", hashlib.sha256(data).hexdigest())
    if len(sys.argv) == 5:
        prefix = sys.argv[4]
        with torch.no_grad():
            vocals = apply_model(model, mix, shifts=0, split=True, overlap=0.25, progress=False)[0]
        mix[0].numpy().astype("<f4").tofile(prefix + ".mix.f32")
        vocals[model.sources.index("vocals")].numpy().astype("<f4").tofile(prefix + ".vocals.f32")
        print(f"wrote {prefix}.mix.f32 and {prefix}.vocals.f32")


if __name__ == "__main__":
    main()
