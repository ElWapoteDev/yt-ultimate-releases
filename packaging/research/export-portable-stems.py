#!/usr/bin/env python3
"""Optional build-time experiment; never invoked or packaged by the application.

Requires the existing development environment's PyTorch 2.12. A reference WAV
also requires soundfile. The published model is read-only and every output must
be new. The low-level graph transformation is deliberately pinned to its SHA-256.
"""
from __future__ import annotations

import argparse
import gc
import hashlib
import json
import math
import os
from pathlib import Path
import tempfile
import time


PUBLISHED_SHA256 = "31d794ab054bd70964b6165ceca3b94e00f344f8a0b8bc325def47ce9aa14926"
FRAMES = 352800
RATE = 44100


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--reference-wav", type=Path)
    parser.add_argument("--start", type=float, default=0)
    parser.add_argument("--mps", action="store_true")
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    if args.destination.exists() or (args.report and args.report.exists()):
        parser.error("Output files must not already exist")
    if not math.isfinite(args.start) or args.start < 0:
        parser.error("--start must be nonnegative and finite")
    if sha256(args.source) != PUBLISHED_SHA256:
        parser.error("The source is not the pinned, published BS-RoFormer graph")

    import torch

    if not torch.__version__.startswith("2.12."):
        parser.error("This graph experiment is validated only with PyTorch 2.12")
    torch.set_num_threads(4)
    if args.reference_wav:
        import soundfile

        with soundfile.SoundFile(args.reference_wav) as source:
            if source.samplerate != RATE or source.channels != 2:
                parser.error("Reference WAV must be stereo, 44100 Hz")
            source.seek(round(args.start * RATE))
            data = source.read(FRAMES, dtype="float32", always_2d=True)
        if len(data) != FRAMES:
            parser.error("Reference WAV must contain eight seconds after --start")
        sample = torch.from_numpy(data.T.copy())[None]
    else:
        torch.manual_seed(1837)
        t = torch.arange(FRAMES, dtype=torch.float32) / RATE
        sample = torch.stack([
            0.17 * torch.sin(2 * math.pi * 220 * t) + 0.03 * torch.randn(FRAMES),
            0.11 * torch.sin(2 * math.pi * 440 * t) + 0.03 * torch.randn(FRAMES),
        ])[None]

    def run(path: Path, device: str, repeats: int = 1):
        started = time.monotonic()
        model = torch.jit.load(str(path), map_location=device).eval()
        loaded = time.monotonic() - started
        data = sample.to(device)
        times = []
        with torch.inference_mode():
            for _ in range(repeats):
                started = time.monotonic()
                result = model(data).cpu()
                if device == "mps":
                    torch.mps.synchronize()
                times.append(time.monotonic() - started)
                if result.shape != sample.shape or not torch.isfinite(result).all():
                    raise RuntimeError("Model returned invalid samples or shape")
        del model, data
        gc.collect()
        if device == "mps":
            torch.mps.empty_cache()
        return result, {"load_seconds": loaded, "inference_seconds": times}

    def difference(reference, result):
        error = reference - result
        energy = error.square().sum().item()
        return {
            "max_abs": error.abs().max().item(),
            "rms": error.square().mean().sqrt().item(),
            "snr_db": 10 * math.log10(reference.square().sum().item() / energy) if energy else None,
            "bit_identical": torch.equal(reference, result),
        }

    report = {"torch": torch.__version__, "source_sha256": PUBLISHED_SHA256}
    reference, report["original_cpu"] = run(args.source, "cpu")
    model = torch.jit.load(str(args.source), map_location="cpu").eval()
    graph = model.graph
    counts = {"double_scalars": 0, "integer_scalars": 0, "fixed_cpu_device": 0}
    for node in list(graph.nodes()):
        if node.kind() != "prim::Constant":
            continue
        value = node.output().toIValue()
        if isinstance(value, torch.Tensor) and value.dtype == torch.float64:
            if value.ndim != 0:
                raise RuntimeError("Unexpected nonscalar float64 constant")
            value = value.float()
            node.t_("value", value)
            node.output().setType(torch._C.TensorType.create_from_tensor(value))
            counts["double_scalars"] += 1
        elif isinstance(value, torch.Tensor) and value.dtype == torch.int64:
            if value.ndim != 0:
                raise RuntimeError("Unexpected nonscalar int64 constant")
            node.removeAttribute("value")
            node.i_("value", value.item())
            node.output().setType(torch._C.IntType.get())
            counts["integer_scalars"] += 1
        elif isinstance(value, torch.device):
            if value.type != "cpu":
                raise RuntimeError("Unexpected fixed device")
            device = graph.create("prim::device", [list(graph.inputs())[1]], 1)
            device.output().setType(torch._C.DeviceObjType.get())
            device.insertBefore(node)
            node.output().replaceAllUsesWith(device.output())
            node.destroy()
            counts["fixed_cpu_device"] += 1
    if counts != {"double_scalars": 8, "integer_scalars": 4, "fixed_cpu_device": 1}:
        raise RuntimeError(f"Unexpected graph structure: {counts}")
    torch._C._jit_pass_lint(graph)
    # This directory is owned by the experiment; an existing destination is
    # never overwritten, even if another process creates it during validation.
    with tempfile.TemporaryDirectory(dir=args.destination.parent, prefix="stems-export-") as temporary:
        candidate = Path(temporary) / "candidate.pt"
        torch.jit.save(model, str(candidate))
        del model, graph
        gc.collect()
        cpu, report["candidate_cpu"] = run(candidate, "cpu")
        report["cpu_difference"] = difference(reference, cpu)
        if not torch.equal(reference, cpu):
            raise RuntimeError("Candidate changed the CPU reference output")
        if args.mps:
            if not torch.backends.mps.is_available():
                raise RuntimeError("Metal is unavailable in this environment")
            gpu, report["candidate_mps"] = run(candidate, "mps", repeats=2)
            report["mps_difference"] = difference(reference, gpu)
        report["candidate_sha256"] = sha256(candidate)
        report["candidate_bytes"] = candidate.stat().st_size
        report["transformation"] = counts
        os.link(candidate, args.destination)
    text = json.dumps(report, indent=2, allow_nan=False) + "\n"
    if args.report:
        with args.report.open("x", encoding="utf-8") as destination:
            destination.write(text)
    print(text, end="")


if __name__ == "__main__":
    main()
