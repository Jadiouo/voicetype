"""Run the installed Windows Nano CPU daemon on a pinned public speech sample.

This is prerecorded inference only. It does not test capture, shortcuts or delivery.
The WAV is used from the pinned upstream model archive in CI; it is not shipped.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import time
import unicodedata
import urllib.request
import wave


ROOT = Path(__file__).resolve().parents[2]
ASSETS = ROOT / "desktop/assets"
SAMPLE = "test_wavs/rag_history.wav"
# Ground truth is published with this sample by sherpa-onnx:
# https://k2-fsa.github.io/sherpa/onnx/funasr-nano/pretrained.html
GROUND_TRUTH = "由罗马皇帝钦点的犹地亚王大希律王统治期间"
# The model transcribes 大希律王 as the homophone 大西律王 (seen in CI run
# 38147181476). That is a model accuracy limit, not a loosened comparison: the
# other three anchors stay exact and the proper name keeps its distinctive "律王".
REPEATS = 5
ANCHORS = ("罗马皇帝", "犹地亚王", "律王", "统治期间")


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(path):
    sha = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            size += len(chunk)
            sha.update(chunk)
    return size, sha.hexdigest()


def pinned_file(path, pin):
    require(path.is_file() and not path.is_symlink(), f"Pinned file missing: {path.name}")
    require(digest(path) == (pin["bytes"], pin["sha256"]), f"Pinned bytes differ: {path.name}")


def obtain(url, pin, destination):
    if destination.exists():
        pinned_file(destination, pin)
        return destination
    require(url.startswith("https://"), "Pinned source must use HTTPS")
    staged = destination.with_name(destination.name + ".part")
    require(not staged.exists(), f"Incomplete prior download: {staged.name}")
    try:
        with urllib.request.urlopen(url, timeout=60) as response, staged.open("xb") as output:
            sha = hashlib.sha256()
            size = 0
            while chunk := response.read(1024 * 1024):
                size += len(chunk)
                require(size <= pin["bytes"], "Download exceeds pinned size")
                sha.update(chunk)
                output.write(chunk)
        require((size, sha.hexdigest()) == (pin["bytes"], pin["sha256"]),
                "Downloaded bytes do not match pinned source")
        staged.replace(destination)
    finally:
        staged.unlink(missing_ok=True)
    return destination


def extract_model_and_sample(archive, model_manifest, prefix, work):
    model = work / "model"
    model.mkdir()
    expected = {f"{prefix}/{item['path']}": (model / item["path"], item)
                for item in model_manifest["files"]}
    sample = work / "rag_history.wav"
    expected[f"{prefix}/{SAMPLE}"] = (sample, None)
    seen = set()
    sample_pin = None
    with tarfile.open(archive, "r|bz2") as source:
        for member in source:
            target = expected.get(member.name)
            if target is None:
                continue
            require(member.name not in seen and member.isfile(), "Duplicate or non-file pinned member")
            seen.add(member.name)
            path, pin = target
            require(member.size <= (pin["bytes"] if pin else 1024 * 1024), "Oversize archive member")
            path.parent.mkdir(parents=True, exist_ok=True)
            content = source.extractfile(member)
            require(content is not None, "Pinned member could not be read")
            sha = hashlib.sha256()
            size = 0
            with path.open("xb") as output:
                while chunk := content.read(1024 * 1024):
                    size += len(chunk)
                    sha.update(chunk)
                    output.write(chunk)
            require(size == member.size, "Truncated pinned member")
            if pin:
                require((size, sha.hexdigest()) == (pin["bytes"], pin["sha256"]),
                        "Model member differs from pinned catalog")
            else:
                sample_pin = (size, sha.hexdigest())
    require(seen == expected.keys(), "Pinned archive lacks model files or public WAV")
    return model, sample, sample_pin


def speech_sample(path):
    with wave.open(str(path), "rb") as wav:
        require((wav.getnchannels(), wav.getsampwidth(), wav.getframerate()) == (1, 2, 16000),
                "Public WAV is not 16-bit PCM mono at 16 kHz")
        seconds = wav.getnframes() / wav.getframerate()
        require(1 <= seconds <= 30, "Public WAV duration outside probe bound")


def cpu_name():
    # Evidence only: int8 ONNX kernels can differ by CPU, and empty output
    # appeared on some hosted runners but not others.
    try:
        out = subprocess.run(["powershell", "-NoProfile", "-Command",
                              "(Get-CimInstance Win32_Processor | Select-Object -First 1).Name"],
                             capture_output=True, timeout=30, check=False)
        return out.stdout.decode("utf-8", "replace").strip()
    except (OSError, subprocess.SubprocessError):
        return "unknown"


def normalized(text):
    return "".join(char for char in text if not char.isspace()
                   and not unicodedata.category(char).startswith("P"))


def main(runtime, scratch, archive_override):
    require(sys.platform == "win32", "Run this probe on a Windows CPU runner")
    root = ROOT.resolve()
    runtime = runtime.resolve(strict=True)
    scratch = scratch.resolve()
    require(runtime.is_relative_to(root) and scratch.is_relative_to(root),
            "Runtime and scratch must stay inside the repository")
    scratch.mkdir(parents=True, exist_ok=True)
    manifest = json.loads((runtime / "manifest.json").read_text(encoding="utf-8"))
    require(manifest["id"] == "nano-runtime" and manifest["platform"] == "windows-x86_64",
            "Not a packaged Windows Nano runtime")
    exe = runtime / "bin/voicetyped.exe"
    members = {item["path"]: item for item in manifest["files"]}
    for relative in ("bin/voicetyped.exe", "bin/sherpa-onnx-c-api.dll", "bin/onnxruntime.dll"):
        pinned_file(runtime / relative, members[relative])

    downloads = json.loads((ASSETS / "downloads.json").read_text(encoding="utf-8"))["downloads"]
    model_manifest = json.loads((ASSETS / "nano-models.json").read_text(encoding="utf-8"))
    vad_manifest = json.loads((ASSETS / "silero-vad.json").read_text(encoding="utf-8"))
    model_pin = next(item for item in downloads if item["manifest"] == "nano-models.json")
    vad_pin = next(item for item in downloads if item["manifest"] == "silero-vad.json")
    archive = archive_override or scratch / "nano-model.tar.bz2"
    if archive_override:
        pinned_file(archive, model_pin)
    else:
        obtain(model_manifest["source_url"], model_pin, archive)
    vad = obtain(vad_manifest["source_url"], vad_pin, scratch / "silero-v5.0.onnx")
    pinned_file(vad, vad_manifest["files"][0])

    with tempfile.TemporaryDirectory(prefix="nano-asr-", dir=scratch) as temporary:
        model, sample, sample_pin = extract_model_and_sample(
            archive, model_manifest, model_pin["strip_prefix"], Path(temporary))
        speech_sample(sample)
        env = os.environ.copy()
        env.update(VOICETYPE_ASR_PROFILE="nano", VOICETYPE_NANO_MODEL_DIR=str(model),
                   VOICETYPE_NANO_VAD_MODEL=str(vad), CUDA_VISIBLE_DEVICES="")
        started = time.monotonic()
        # Run the installed daemon several times: an earlier CI run returned an
        # empty transcript once for the same bytes, so frequency matters.
        transcripts, failures = [], []
        for attempt in range(1, REPEATS + 1):
            result = subprocess.run([str(exe), "--transcribe", str(sample)], cwd=runtime / "bin",
                                    env=env, capture_output=True, timeout=120, check=False)
            require(result.returncode == 0,
                    f"Nano process exited {result.returncode}: {result.stderr[-2048:].decode('utf-8', 'replace')}")
            require(len(result.stdout) <= 16 * 1024, "Nano transcript output unexpectedly large")
            transcript = result.stdout.decode("utf-8").strip()
            transcripts.append(transcript)
            if not (transcript and all(anchor in normalized(transcript) for anchor in ANCHORS)):
                # Diagnostic only: the probe still fails. --no-vad separates the
                # VAD gate (empty output) from the recognizer.
                novad = subprocess.run([str(exe), "--transcribe", str(sample), "--no-vad"], cwd=runtime / "bin",
                                       env=env, capture_output=True, timeout=120, check=False)
                failures.append(
                    f"attempt {attempt}: {transcript!r}; "
                    f"stderr tail: {result.stderr[-1500:].decode('utf-8', 'replace')!r}; "
                    f"--no-vad exit {novad.returncode} output: {novad.stdout[:400].decode('utf-8', 'replace')!r}; "
                    f"--no-vad stderr tail: {novad.stderr[-800:].decode('utf-8', 'replace')!r}")
        if failures:
            raise RuntimeError(f"Nano did not recognize the pinned public phrase in "
                               f"{len(failures)}/{REPEATS} runs on CPU {cpu_name()!r}: " + " | ".join(failures))
        require("provider=cpu" in result.stderr.decode("utf-8", "replace"),
                "Native Nano did not report CPU provider")
        print(json.dumps({"prerecorded_nano_cpu_asr": True,
                          "sample": SAMPLE, "sample_bytes": sample_pin[0],
                          "sample_sha256": sample_pin[1], "archive_sha256": model_pin["sha256"],
                          "ground_truth": GROUND_TRUTH, "transcript": transcripts[0], "runs": REPEATS, "cpu": cpu_name(),
                          "process_elapsed_seconds": round(time.monotonic() - started, 2)},
                         ensure_ascii=True))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", type=Path, required=True, help="installed package runtime directory")
    parser.add_argument("--scratch", type=Path, required=True, help="repository-local scratch directory")
    parser.add_argument("--archive", type=Path, help="existing pinned upstream archive; avoids model download")
    args = parser.parse_args()
    try:
        main(args.runtime, args.scratch, args.archive)
    except (OSError, ValueError, RuntimeError, KeyError, StopIteration, subprocess.TimeoutExpired,
            tarfile.TarError, wave.Error) as error:
        parser.exit(1, ascii(f"Windows prerecorded Nano ASR probe failed: {error}") + "\n")
