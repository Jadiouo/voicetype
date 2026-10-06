#!/usr/bin/env python3
"""Exercise the running daemon's real text pipeline, without recording or typing.

Uses the normal default mode (does not force model off). The installed profile
must therefore be checked separately. Measures IPC + text processing only,
never microphone startup, ASR, or desktop delivery latency.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import statistics
import time

CONTROL = Path(__file__).resolve().parents[1] / "scripts/voicetype-control.py"
spec = importlib.util.spec_from_file_location("control", CONTROL)
control = importlib.util.module_from_spec(spec)
spec.loader.exec_module(control)

CASES = [
    ("mixed", "把應用程式的 comimt 和 pull requset 推到 gthub。",
     "把應用程式的 commit 和 pull request 推到 GitHub。"),
    ("proper_name", "最後核對 git hub。", "最後核對 GitHub。"),
    ("valid_english", "integrative research, try catch, the human brain.", None),
    ("already_correct", "VoiceType 和 GitHub 的 pull request。", None),
    ("negation_numbers", "不要刪除 120、8GB、0.5 秒或第 2 個 commit。", None),
    ("repetition", "應用程式，應用程式。不要、不要刪除。",
     "應用程式，應用程式。不要、不要刪除。"),
    ("case_boundary", "把GTHUB與git hub送出去，不動mygthub和gthub_id。",
     "把GitHub與GitHub送出去，不動mygthub和gthub_id。"),
    ("literal", "`gthub` /tmp/gthub gthub_id gthub", "`gthub` /tmp/gthub gthub_id GitHub"),
    ("fenced_code", "```text\ngthub\n``` 外面 gthub", "```text\ngthub\n``` 外面 GitHub"),
    ("traditional", "这个应用程式不要删掉第 120 个 commit。",
     "這個應用程式不要刪掉第 120 個 commit。"),
    ("aliases", "commmit 這個 pull reqeust。", "commit 這個 pull request。"),
    ("existing_vocabulary", "艾薩克心和熱力瑞。", "Isaac Sim和learning rate。"),
]


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--socket")
    ap.add_argument("--output", required=True, type=Path)
    ap.add_argument("--baseline", action="store_true", help="Record pre-change results without a pass requirement")
    args = ap.parse_args()
    rows = []
    for repetition in range(3):
        for name, text, expected in CASES:
            expected = text if expected is None else expected
            start = time.perf_counter()
            result = control.request({"type": "process_text", "text": text}, args.socket)
            elapsed = (time.perf_counter() - start) * 1000
            actual = result["text"]
            rows.append(dict(id=name, repetition=repetition, input=text, expected=expected,
                             actual=actual, correct=actual == expected, elapsed_ms=elapsed))
    warm = [r["elapsed_ms"] for r in rows[len(CASES):]]
    report = dict(cases=rows, correct=sum(r["correct"] for r in rows), total=len(rows),
                  first_request_ms=rows[0]["elapsed_ms"], warm_median_ms=statistics.median(warm),
                  warm_max_ms=max(warm), boundary="Unix IPC + common text output pipeline; no ASR or insertion")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    args.output.chmod(0o600)
    print(json.dumps({k: v for k, v in report.items() if k != "cases"}, ensure_ascii=False))
    if not args.baseline and report["correct"] != len(rows):
        for row in rows:
            if not row["correct"]:
                print(json.dumps(row, ensure_ascii=False))
        raise SystemExit(1)


if __name__ == "__main__":
    main()
