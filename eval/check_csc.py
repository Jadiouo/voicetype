#!/usr/bin/env python3
"""Evaluate fixed text cases over real worker/daemon IPC without audio or typing."""
import argparse
import collections
import hashlib
import importlib.util
import json
from pathlib import Path
import statistics
import time

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("control", ROOT / "scripts/voicetype-control.py")
control = importlib.util.module_from_spec(spec)
spec.loader.exec_module(control)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    group = ap.add_mutually_exclusive_group(required=True)
    group.add_argument("--worker-socket")
    group.add_argument("--daemon-socket")
    ap.add_argument("--dataset", type=Path, required=True)
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--repetitions", type=int, default=3)
    ap.add_argument("--compare-off", action="store_true",
                    help="For daemon keep cases, compare against its existing off-mode conversion; still report raw mismatches")
    args = ap.parse_args()
    if not 1 <= args.repetitions <= 20:
        ap.error("repetitions must be between 1 and 20")
    if args.compare_off and not args.daemon_socket:
        ap.error("--compare-off requires --daemon-socket")
    dataset = json.loads(args.dataset.read_text())
    rows = []
    for repetition in range(args.repetitions):
        for case in dataset["cases"]:
            text = case["text"]
            if args.worker_socket:
                message = dict(v=1, id=len(rows)+1, text=text, terms=case.get("terms", []),
                               sent_at_ms=int(time.time()*1000))
            else:
                message = dict(type="process_text", text=text)
            start = time.perf_counter()
            result = control.request(message, args.worker_socket or args.daemon_socket)
            elapsed = (time.perf_counter()-start)*1000
            if args.worker_socket:
                assert result["id"] == message["id"] and result["v"] == 1
                characters = list(text)
                for edit in result["edits"]:
                    assert characters[edit["start"]] == edit["source"]
                    assert len(edit["target"]) == 1
                    characters[edit["start"]] = edit["target"]
                actual = "".join(characters)
            else:
                actual = result["text"]
            baseline = text
            if args.compare_off:
                # The prediction is already finished. The baseline bypasses the
                # model and never submits any reference to it or learns a rule.
                baseline = control.request(dict(type="process_text", text=text, mode="off"),
                                           args.daemon_socket)["text"]
            expected = baseline if args.compare_off and case["kind"] == "keep" else case["expected"]
            rows.append(dict(**case, repetition=repetition, actual=actual, elapsed_ms=elapsed,
                             correct=actual == expected, raw_expected_match=actual == case["expected"],
                             baseline=baseline, baseline_changed=baseline != text,
                             changed=actual != baseline,
                             status=result.get("status"), model_calls=result.get("model_calls")))
    summary = {}
    for kind in sorted({r["kind"] for r in rows}):
        subset = [r for r in rows if r["kind"] == kind]
        summary[kind] = dict(correct=sum(r["correct"] for r in subset), total=len(subset),
                             changed=sum(r["changed"] for r in subset),
                             raw_expected_match=sum(r["raw_expected_match"] for r in subset),
                             baseline_changed=sum(r["baseline_changed"] for r in subset))
    durations = sorted(r["elapsed_ms"] for r in rows)
    summary.update(first_request_ms=rows[0]["elapsed_ms"], p50_ms=statistics.median(durations),
                   p95_ms=durations[min(len(durations)-1, int(.95*len(durations)))],
                   max_ms=max(durations), statuses=dict(collections.Counter(r["status"] for r in rows)))
    report = dict(dataset_sha256=hashlib.sha256(args.dataset.read_bytes()).hexdigest(),
                  boundary="worker IPC" if args.worker_socket else "daemon IPC + common text output pipeline",
                  compare_off=args.compare_off,
                  audio=False, insertion=False, summary=summary, cases=rows)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    # Exclusive creation prevents silently replacing a first/frozen evaluation.
    with args.output.open("x") as stream:
        json.dump(report, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
    args.output.chmod(0o600)
    print(json.dumps(summary, ensure_ascii=False))
    for row in rows:
        if row["kind"] == "keep" and not row["correct"]:
            print(json.dumps(row, ensure_ascii=False))
    if any(r["kind"] == "keep" and not r["correct"] for r in rows):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
