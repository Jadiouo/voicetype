#!/usr/bin/env python3
"""Prepare the pinned author's MacBERT ONNX for CPU spelling correction.

Requires config/csc-requirements.txt plus onnx==1.20.1. Does not install services.
The optional source directory reuses already downloaded, hash-verified files.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import urllib.request

REVISION = "615e6e09ef9a69ec487bc7c641ec3a311e2c11b9"
BASE = f"https://huggingface.co/shibing624/macbert4csc-base-chinese/resolve/{REVISION}/onnx"
SOURCES = {
    "model.onnx": "a4d9d6807c0c8bc9d014462a728e7212315630c669a6bd670b653154391a73f3",
    "tokenizer.json": "7dfbf1966ebf99d471c3796e9b457329d2b2182b817e144f1e904b957745c839",
}


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--output-dir", required=True, type=Path)
    ap.add_argument("--source-dir", type=Path)
    args = ap.parse_args()
    root = args.output_dir
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    final = root / "model-int8-fused.onnx"
    if final.exists():
        ap.error("output model already exists; use a fresh output directory")
    for name, expected in SOURCES.items():
        path = root / name
        if not path.exists():
            partial = path.with_suffix(path.suffix + ".part")
            if args.source_dir:
                shutil.copyfile(args.source_dir / name, partial)
            else:
                with urllib.request.urlopen(f"{BASE}/{name}", timeout=60) as response:
                    with partial.open("wb") as stream:
                        shutil.copyfileobj(response, stream)
            if digest(partial) != expected:
                raise SystemExit(f"Source hash mismatch: {name}")
            partial.rename(path)
        if digest(path) != expected:
            raise SystemExit(f"Source hash mismatch: {name}")

    import onnx
    import onnxruntime as ort
    from onnxruntime.quantization import quantize_dynamic, QuantType
    from onnxruntime.transformers.fusion_options import FusionOptions
    from onnxruntime.transformers.optimizer import optimize_model

    basic = root / "model-basic.onnx"
    options = ort.SessionOptions()
    options.intra_op_num_threads = 4
    options.inter_op_num_threads = 1
    options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_BASIC
    options.optimized_model_filepath = str(basic)
    session = ort.InferenceSession(str(root / "model.onnx"), options,
                                  providers=["CPUExecutionProvider"])
    if session.get_providers() != ["CPUExecutionProvider"]:
        raise SystemExit("CPU-only provider required")
    del session
    fusion = FusionOptions("bert")
    fusion.enable_embed_layer_norm = False
    # ORT did the basic CPU pass above. opt_level=0 prevents the transformer
    # helper from trying to select a device or importing torch unnecessarily.
    optimized = optimize_model(str(basic), model_type="bert", num_heads=12,
                               hidden_size=768, optimization_options=fusion,
                               opt_level=0, use_gpu=False)
    fused = root / "model-fused.onnx"
    optimized.save_model_to_file(str(fused))
    stats = optimized.get_fused_operator_statistics()
    del optimized
    quantize_dynamic(str(fused), str(final), per_channel=True,
                     weight_type=QuantType.QInt8,
                     op_types_to_quantize=["MatMul", "Gemm", "Attention"],
                     extra_options={"DefaultTensorType": onnx.TensorProto.FLOAT})
    manifest = dict(source_revision=REVISION, source_hashes=SOURCES,
                    provider="CPUExecutionProvider", ort=ort.__version__, onnx=onnx.__version__,
                    fused_operators=stats, model_sha256=digest(final),
                    tokenizer_sha256=digest(root / "tokenizer.json"))
    (root / "prepared.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
