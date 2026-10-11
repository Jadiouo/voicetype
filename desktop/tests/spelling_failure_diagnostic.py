"""Bounded, text-free diagnostics for a failed installed CPU spelling probe.

Run only after spelling_bundle fails. This talks to a fresh frozen child directly,
so its status separates engine deadline/no-edit from owner pipe behavior. It does
not change the production 100 ms correction limit or turn a failed CI gate green.
"""
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading
import time


def receive(lines, timeout):
    try:
        line = lines.get(timeout=timeout)
    except queue.Empty as error:
        raise TimeoutError("frozen child did not reply") from error
    if line is None:
        raise EOFError("frozen child closed its protocol pipe")
    return json.loads(line)


def read_lines(stream, lines):
    for line in stream:
        lines.put(line)
    lines.put(None)


def main():
    bundle = Path(sys.argv[1]).resolve(strict=True)
    manifest = json.loads((bundle / "manifest.json").read_text())
    files = {entry["path"]: entry for entry in manifest["files"]}
    executable = bundle / ("voicetype-csc.exe" if os.name == "nt" else "voicetype-csc")
    model = bundle / "models/model-int8-fused.onnx"
    tokenizer = bundle / "models/tokenizer.json"
    environment = {key: value for key, value in os.environ.items()
                   if key in ("SystemRoot", "WINDIR", "TEMP", "TMP")}
    environment.update(CUDA_VISIBLE_DEVICES="", LANG="C.UTF-8")
    command = [str(executable), "--stdio", "--model", str(model),
               "--tokenizer", str(tokenizer), "--model-sha256",
               files["models/model-int8-fused.onnx"]["sha256"], "--tokenizer-sha256",
               files["models/tokenizer.json"]["sha256"], "--threads", "4"]
    child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL, env=environment)
    lines = queue.Queue()
    threading.Thread(target=read_lines, args=(child.stdout, lines), daemon=True).start()
    original = "今天新情很好。GitHub 2026"
    expected = "今天心情很好。GitHub 2026"
    try:
        started = time.monotonic()
        ready = receive(lines, 30)
        print(json.dumps({"diagnostic": "ready", "status": ready.get("status"),
                          "provider": ready.get("provider"),
                          "elapsed_ms": round((time.monotonic() - started) * 1000)}), flush=True)
        if ready != {"v": 1, "status": "ready", "provider": "CPUExecutionProvider"}:
            raise ValueError("unexpected frozen child readiness")
        for attempt in range(1, 4):
            request = {"v": 1, "id": attempt, "text": original, "terms": [],
                       "sent_at_ms": int(time.time() * 1000)}
            started = time.monotonic()
            child.stdin.write(json.dumps(request, ensure_ascii=False).encode() + b"\n")
            child.stdin.flush()
            reply = receive(lines, 20)
            applied = list(original)
            for edit in reply.get("edits", []):
                index = edit["start"]
                if applied[index] == edit["source"]:
                    applied[index] = edit["target"]
            print(json.dumps({"diagnostic": "direct_request", "attempt": attempt,
                              "status": reply.get("status"),
                              "model_calls": reply.get("model_calls"),
                              "edits": len(reply.get("edits", [])),
                              "matches_expected": "".join(applied) == expected,
                              "elapsed_ms": round((time.monotonic() - started) * 1000)}),
                  flush=True)
    except (EOFError, TimeoutError, ValueError, KeyError, IndexError, OSError) as error:
        print(json.dumps({"diagnostic": "error", "type": type(error).__name__,
                          "message": str(error), "child_exit": child.poll()}), flush=True)
    finally:
        if child.poll() is None:
            child.terminate()
        try:
            child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=3)


if __name__ == "__main__":
    main()
