#!/usr/bin/env python3
"""Evaluate the real daemon's TEXT pipeline; this is not an ASR/audio benchmark.

    python3 eval/evaluate_context.py --socket /path/to/ipc.sock --json /tmp/context.json

Only process_text requests are sent via scripts/voicetype-control.py. No recording,
typing, learning, or persistent context updates. Use an isolated empty learning
store and no manual context for reproducibility. Run against the intended local
model, not a stub, when reporting model quality. Unchanged output is reported as
a miss on fix cases: an offline/disabled model cannot earn helpful-fix credit.

Exit codes: 0 no false corrections/errors; 1 false corrections; 2 input/IPC errors;
3 fewer complete fixes than an explicitly requested --require-fixes threshold.
"""

import argparse
import importlib.util
import itertools
import json
import math
from pathlib import Path
import statistics
import sys
import time
import uuid


ROOT = Path(__file__).resolve().parent.parent


def load_control():
    spec = importlib.util.spec_from_file_location(
        "voicetype_control", ROOT / "scripts" / "voicetype-control.py"
    )
    if spec is None or spec.loader is None:
        raise ValueError("Cannot load scripts/voicetype-control.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def expected_variants(case):
    """Return exact accepted partial outputs and their desired-edit counts.

    No normalization: unrequested punctuation, casing, or whitespace changes
    count as false corrections. Edits use original spans, never cascades.
    """
    text = case["text"]
    edits = case.get("required_edits", [])
    if case["kind"] == "keep":
        if edits:
            raise ValueError(f"{case['id']}: keep case cannot declare edits")
        return {text: 0}, text
    if not 1 <= len(edits) <= 8:
        raise ValueError(f"{case['id']}: fix case requires 1..8 desired edits")
    spans = []
    for edit in edits:
        old, new = edit.get("from"), edit.get("to")
        if not isinstance(old, str) or not isinstance(new, str):
            raise ValueError(f"{case['id']}: edit from/to must be strings")
        if not old or old == new or text.count(old) != 1:
            raise ValueError(f"{case['id']}: source span must uniquely change text")
        start = text.index(old)
        spans.append((start, start + len(old), new))
    spans.sort()
    if any(left[1] > right[0] for left, right in zip(spans, spans[1:])):
        raise ValueError(f"{case['id']}: overlapping desired edits")

    variants = {}
    full = None
    for selected in itertools.product((False, True), repeat=len(spans)):
        output, offset = [], 0
        for enabled, (start, end, replacement) in zip(selected, spans):
            output.extend((text[offset:start], replacement if enabled else text[start:end]))
            offset = end
        output.append(text[offset:])
        candidate = "".join(output)
        variants[candidate] = sum(selected)
        if all(selected):
            full = candidate
    return variants, full


def load_cases(path):
    suite = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(suite, dict) or suite.get("schema_version") != 1:
        raise ValueError("Unsupported context case schema")
    cases = suite.get("cases")
    if not isinstance(cases, list) or not cases:
        raise ValueError("Suite needs a nonempty cases list")
    seen = set()
    for case in cases:
        if not isinstance(case, dict):
            raise ValueError("Each case must be an object")
        case_id = case.get("id")
        if not isinstance(case_id, str) or not case_id or case_id in seen:
            raise ValueError("Every case needs a unique, nonempty id")
        seen.add(case_id)
        if case.get("kind") not in ("fix", "keep") or not isinstance(case.get("text"), str):
            raise ValueError(f"{case_id}: need kind=fix|keep and text")
        for field in ("context_text", "selected_text", "reason"):
            if field in case and not isinstance(case[field], str):
                raise ValueError(f"{case_id}: {field} must be a string")
        if case.get("mode", "faithful") not in ("off", "faithful", "clean"):
            raise ValueError(f"{case_id}: invalid mode")
        expected_variants(case)
    return suite


def classify(case, output):
    variants, full = expected_variants(case)
    if case["kind"] == "keep":
        return ("preserved", 0) if output == case["text"] else ("false_correction", 0)
    if output == full:
        return "helpful_fix", len(case["required_edits"])
    if output == case["text"]:
        return "unchanged_miss", 0
    if output in variants:
        return "partial_fix", variants[output]
    return "false_correction", 0


def summarize(rows):
    labels = ("helpful_fix", "partial_fix", "preserved", "unchanged_miss", "false_correction", "error")
    counts = {label: sum(row["classification"] == label for row in rows) for label in labels}
    successful = [row["elapsed_ms"] for row in rows if row["classification"] != "error"]
    fix_cases = sum(row["kind"] == "fix" for row in rows)
    keep_cases = len(rows) - fix_cases
    return {
        "requests": len(rows),
        "fix_cases": fix_cases,
        "keep_cases": keep_cases,
        **counts,
        "desired_edits": sum(row["desired_edits"] for row in rows),
        "helpful_edits": sum(row["helpful_edits"] for row in rows),
        "keep_false_corrections": sum(
            row["kind"] == "keep" and row["classification"] == "false_correction" for row in rows
        ),
        "elapsed_ms_p50": round(statistics.median(successful), 2) if successful else None,
        "elapsed_ms_p95": round(sorted(successful)[math.ceil(len(successful) * .95) - 1], 2) if successful else None,
        "latency_method": "Round-trip wall time; successful responses only; p50 median, p95 nearest rank.",
        "helpfulness_demonstrated": counts["helpful_fix"] + counts["partial_fix"] > 0,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--socket", help="Daemon UNIX socket; defaults to voicetype-control's normal path")
    parser.add_argument("--cases", type=Path, default=Path(__file__).with_name("context_cases.json"))
    parser.add_argument("--case", action="append", default=[], help="Run a named case; may be repeated")
    parser.add_argument("--json", type=Path, help="Write exact per-case inputs, expected and actual outputs")
    parser.add_argument("--repeat", type=int, default=1, help="Repeat each case (1..10), retaining every outcome")
    parser.add_argument("--require-fixes", type=int, default=0, help="Minimum number of complete fixes across all repetitions")
    parser.add_argument("--label", default="", help="Record model/runtime settings supplied by the operator; not auto-verified")
    args = parser.parse_args()
    if not 1 <= args.repeat <= 10 or args.require_fixes < 0:
        parser.error("--repeat must be 1..10 and --require-fixes nonnegative")

    suite = load_cases(args.cases)
    cases = suite["cases"]
    known = {case["id"] for case in cases}
    unknown = set(args.case) - known
    if unknown:
        parser.error("Unknown cases: " + ", ".join(sorted(unknown)))
    if args.case:
        cases = [case for case in cases if case["id"] in args.case]
    control = load_control()
    run_id = uuid.uuid4().hex[:12]
    rows = []
    print("TEXT PIPELINE ONLY — no microphone/audio/ASR accuracy is measured.", flush=True)
    print("Use isolated learning state and no manual context; no state is modified by this evaluator.", flush=True)
    for repetition in range(args.repeat):
        for case in cases:
            message = {
                "type": "process_text",
                "text": case["text"],
                "context_text": case.get("context_text", ""),
                "selected_text": case.get("selected_text", ""),
                "program": "voicetype-context-eval",
                "context_id": f"text-eval:{run_id}:{case['id']}:{repetition}",
                "mode": case.get("mode", "faithful"),
            }
            row = {
                "id": case["id"], "repetition": repetition + 1, "kind": case["kind"],
                "request": message, "expected": expected_variants(case)[1],
                "desired_edits": len(case.get("required_edits", [])), "helpful_edits": 0,
                "reason": case.get("reason", ""),
            }
            start = time.perf_counter()
            try:
                result = control.request(message, args.socket)
                if not isinstance(result, dict) or not isinstance(result.get("text"), str):
                    raise ValueError("process_text response must contain a text string")
                row["output"] = result["text"]
                row["classification"], row["helpful_edits"] = classify(case, result["text"])
            except (OSError, RuntimeError, ValueError) as exc:
                row["classification"] = "error"
                row["error"] = str(exc)
            row["elapsed_ms"] = round((time.perf_counter() - start) * 1000, 2)
            rows.append(row)
            print(f"{row['classification']:>16}  {case['id']:<36} {row['elapsed_ms']:>8.2f} ms", flush=True)
            if row["classification"] == "false_correction":
                print("  expected:", json.dumps(row["expected"], ensure_ascii=False), flush=True)
                print("  actual:  ", json.dumps(row["output"], ensure_ascii=False), flush=True)
            elif row["classification"] == "error":
                print("  error:", row["error"], flush=True)

    summary = summarize(rows)
    report = {
        "schema_version": 1, "scope": suite["scope"], "run_id": run_id,
        "label_unverified": args.label, "socket": args.socket, "suite": str(args.cases.resolve()),
        "notes": suite.get("notes", []), "summary": summary, "results": rows,
    }
    if args.json:
        args.json.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, ensure_ascii=False, indent=2))
    if not summary["helpfulness_demonstrated"]:
        print("No helpful correction demonstrated. Preserving text alone is not evidence the model is working.")
    if summary["error"]:
        return 2
    if summary["false_correction"]:
        return 1
    if summary["helpful_fix"] < args.require_fixes:
        return 3
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError) as exc:
        print(f"Context evaluation: {exc}", file=sys.stderr)
        sys.exit(2)
