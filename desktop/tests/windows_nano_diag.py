"""Diagnostic only: Nano CPU transcription on one runner under ORT variants.

Records CPU model/flags and the transcript per variant. Never fails on empty
output; it reports. Reuses the pinned downloads of windows_nano_asr.py.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
import windows_nano_asr as probe  # noqa: E402

VARIANTS = {
    "baseline": None,
    "x64quant1": "session.x64quantprecision=1",
    "optlevel0": "GraphOptimizationLevel=0",
    "optlevel1": "GraphOptimizationLevel=1",
}


def cpu_flags():
    try:
        import cpuinfo
        info = cpuinfo.get_cpu_info()
        wanted = [f for f in info.get("flags", []) if f.startswith(("avx", "amx", "fma", "sse4"))]
        return info.get("brand_raw", ""), sorted(wanted)
    except Exception as error:  # evidence only
        return "", [f"cpuinfo failed: {error}"]


def main(runtime, scratch, repeats, variants):
    root = probe.ROOT.resolve()
    runtime, scratch = runtime.resolve(strict=True), scratch.resolve()
    scratch.mkdir(parents=True, exist_ok=True)
    exe = runtime / "bin/voicetyped.exe"
    downloads = json.loads((probe.ASSETS / "downloads.json").read_text(encoding="utf-8"))["downloads"]
    model_manifest = json.loads((probe.ASSETS / "nano-models.json").read_text(encoding="utf-8"))
    vad_manifest = json.loads((probe.ASSETS / "silero-vad.json").read_text(encoding="utf-8"))
    model_pin = next(i for i in downloads if i["manifest"] == "nano-models.json")
    vad_pin = next(i for i in downloads if i["manifest"] == "silero-vad.json")
    archive = probe.obtain(model_manifest["source_url"], model_pin, scratch / "nano-model.tar.bz2")
    vad = probe.obtain(vad_manifest["source_url"], vad_pin, scratch / "silero-v5.0.onnx")
    name = probe.cpu_name()
    brand, flags = cpu_flags()
    print("DIAG_CPU " + json.dumps({"wmi": name, "brand": brand, "flags": flags}, ensure_ascii=True), flush=True)
    with tempfile.TemporaryDirectory(prefix="nano-diag-", dir=scratch) as tmp:
        model, sample, _ = probe.extract_model_and_sample(archive, model_manifest, model_pin["strip_prefix"], Path(tmp))
        for variant in variants:
            env = os.environ.copy()
            env.update(VOICETYPE_ASR_PROFILE="nano", VOICETYPE_NANO_MODEL_DIR=str(model),
                       VOICETYPE_NANO_VAD_MODEL=str(vad), CUDA_VISIBLE_DEVICES="")
            line = VARIANTS[variant]
            if line:
                config = Path(tmp) / f"{variant}.config"
                config.write_text(line.replace("session.", "SessionConfig.session.") + "\n", encoding="utf-8")
                env["VOICETYPE_NANO_ORT_CONFIG"] = str(config)
            for attempt in range(1, repeats + 1):
                started = time.monotonic()
                run = subprocess.run([str(exe), "--transcribe", str(sample)], cwd=runtime / "bin",
                                     env=env, capture_output=True, timeout=300, check=False)
                text = run.stdout.decode("utf-8", "replace").strip()
                ok = bool(text) and all(a in probe.normalized(text) for a in probe.ANCHORS)
                err = run.stderr.decode("utf-8", "replace")
                reason = next((l for l in err.splitlines() if "Nano integrity" in l or "ort_config" in l), "")
                print("DIAG_RESULT " + json.dumps({
                    "runner": os.environ.get("RUNNER_NAME", ""), "os": os.environ.get("ImageOS", ""),
                    "cpu": name, "variant": variant, "attempt": attempt, "exit": run.returncode,
                    "ok": ok, "transcript": text, "seconds": round(time.monotonic() - started, 1),
                    "stderr_hint": reason[-300:]}, ensure_ascii=True), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--scratch", type=Path, required=True)
    parser.add_argument("--repeats", type=int, default=2)
    parser.add_argument("--variants", default="baseline,x64quant1,optlevel0,optlevel1")
    args = parser.parse_args()
    main(args.runtime, args.scratch, args.repeats, args.variants.split(","))
