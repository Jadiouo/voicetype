#!/usr/bin/env python3
"""Compare the text pipeline on saved hypotheses from real-audio ASR evaluation.

    .venv/bin/python eval/evaluate_postproc.py \
        --input /tmp/voicetype-inspect-daemon-noitn.json \
        --socket /path/to/isolated/ipc.sock --json /tmp/postproc.json

This replays evaluate.py's saved hypothesis strings. It does NOT decode audio
again, benchmark a new ASR model, or measure recording-to-typing latency. The
references are used only locally for scoring and NEVER sent as context.

Use a freshly started isolated daemon with an empty learning store and no manual
context. The learning store is checked; absence of manual context is an operator
precondition because IPC does not expose it. Only read-only list_learned and
process_text requests are sent. Mode off still applies the normal traditional
conversion and vocabulary pipeline; faithful adds the optional local refiner.

Exit codes: 0 no worsened CER; 1 at least one worsened case; 2 input/IPC error.
An unchanged result does not prove that the optional model was available.
"""

import argparse
import hashlib
import importlib.util
import json
import math
from pathlib import Path
import statistics
import sys
import time
import uuid

from evaluate import levenshtein, normalize, to_chars


ROOT = Path(__file__).resolve().parent.parent
MAX_ITEMS = 10000
MAX_INPUT_BYTES = 16 * 1024 * 1024


def load_control():
    spec = importlib.util.spec_from_file_location(
        "voicetype_control", ROOT / "scripts" / "voicetype-control.py"
    )
    if spec is None or spec.loader is None:
        raise ValueError("Cannot load scripts/voicetype-control.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def load_baseline(path):
    if path.stat().st_size > MAX_INPUT_BYTES:
        raise ValueError("Input exceeds the 16 MiB evaluation limit")
    encoded = path.read_bytes()
    groups = json.loads(encoded)
    if not isinstance(groups, list) or not groups:
        raise ValueError("Input must be evaluate.py's nonempty list of result sets")
    rows, seen = [], set()
    for group in groups:
        if not isinstance(group, dict) or not isinstance(group.get("set"), str):
            raise ValueError("Each result set needs a set name")
        if not isinstance(group.get("items"), list):
            raise ValueError(f"{group['set']}: missing items list")
        for item in group["items"]:
            if not isinstance(item, dict):
                raise ValueError("Every ASR item must be an object")
            index = item.get("index")
            if isinstance(index, bool) or not isinstance(index, int) or index < 1:
                raise ValueError("Every ASR item needs a positive integer index")
            key = (group["set"], index)
            if key in seen:
                raise ValueError(f"Duplicate ASR item: {key}")
            seen.add(key)
            for field in ("ref", "hyp"):
                if not isinstance(item.get(field), str) or len(item[field]) > 4096:
                    raise ValueError(f"{key}: {field} must be a string of at most 4096 characters")
            rows.append({"set": group["set"], "index": index,
                         "ref": item["ref"], "hyp": item["hyp"]})
    if not 1 <= len(rows) <= MAX_ITEMS:
        raise ValueError(f"Need 1..{MAX_ITEMS} evaluated hypotheses")
    return rows, hashlib.sha256(encoded).hexdigest()


def errors(reference_chars, hypothesis):
    return levenshtein(reference_chars, to_chars(normalize(hypothesis)))[0]


def latency(values):
    if not values:
        return {"count": 0, "p50_ms": None, "p95_ms": None, "max_ms": None}
    ordered = sorted(values)
    return {
        "count": len(values),
        "p50_ms": round(statistics.median(ordered), 2),
        "p95_ms": round(ordered[math.ceil(len(ordered) * .95) - 1], 2),
        "max_ms": round(ordered[-1], 2),
    }


def summarize(rows):
    complete = [r for r in rows if r.get("classification") != "error"]
    chars = sum(r["reference_chars"] for r in complete)
    result = {
        "items": len(rows),
        "completed_pairs": len(complete),
        "errors": len(rows) - len(complete),
        "reference_chars": chars,
        "fix": sum(r["classification"] == "fix" for r in complete),
        "worse": sum(r["classification"] == "worse" for r in complete),
        "unchanged_error_count": sum(r["classification"] == "unchanged" for r in complete),
        "exact_output_unchanged": sum(r["off"]["text"] == r["faithful"]["text"] for r in complete),
        "changed_output_same_error_count": sum(
            r["classification"] == "unchanged" and r["off"]["text"] != r["faithful"]["text"]
            for r in complete
        ),
        "initially_correct_off": sum(r["off"]["errors"] == 0 for r in complete),
        "initially_correct_off_worsened": sum(r["candidate_false_correction"] for r in complete),
        "initially_correct_raw": sum(r["raw_errors"] == 0 for r in complete),
        "initially_correct_raw_worsened": sum(r["raw_correct_regressed"] for r in complete),
        "off_latency": latency([r["off"]["elapsed_ms"] for r in complete]),
        "faithful_latency": latency([r["faithful"]["elapsed_ms"] for r in complete]),
        "faithful_minus_off_latency": latency([
            r["faithful"]["elapsed_ms"] - r["off"]["elapsed_ms"] for r in complete
        ]),
    }
    for mode in ("raw", "off", "faithful"):
        count = sum(r["raw_errors"] if mode == "raw" else r[mode]["errors"] for r in complete)
        result[mode + "_errors"] = count
        result[mode + "_cer"] = 100.0 * count / chars if chars else None
    return result


def report_for(rows, args, input_hash, run_id, total, elapsed):
    names = list(dict.fromkeys(row["set"] for row in rows))
    return {
        "schema_version": 1,
        "scope": "Saved real-audio ASR hypotheses through text postprocessing only; no fresh audio decoding",
        "input": str(args.input.resolve()), "input_sha256": input_hash,
        "socket": args.socket, "run_id": run_id,
        "planned_items": total, "elapsed_seconds": round(elapsed, 3),
        "notes": [
            "References are used only by local scoring; requests contain hypothesis text with empty context/selection.",
            "Off and faithful both run the standard traditional conversion and vocabulary pipeline.",
            "Empty learned store was checked before evaluation; no manual context is an operator precondition.",
            "Scoring imports evaluate.py normalize/to_chars/levenshtein: punctuation, whitespace, case and traditional/simplified are normalized.",
            "CER is summed edit distance divided by summed reference characters, not mean sentence CER.",
            "Initially correct means normalized CER=0; candidate false corrections still need semantic inspection.",
            "Output equality does not prove that a refiner was available; correlate daemon status logs.",
            "Latency is sequential request round-trip wall time, not ASR or recording-to-typing latency; p95 uses nearest rank.",
            "Input evaluate.py JSON does not encode the original ASR model/backend or its decoding settings.",
        ],
        "summary": summarize(rows),
        "sets": [{"set": name, **summarize([r for r in rows if r["set"] == name])}
                 for name in names],
        "candidate_false_corrections": [
            {"set": r["set"], "index": r["index"], "ref": r["ref"],
             "off": r["off"]["text"], "faithful": r["faithful"]["text"]}
            for r in rows if r.get("candidate_false_correction")
        ],
        "results": rows,
    }


def save_report(report, path):
    if path:
        path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--input", required=True, type=Path, help="Existing evaluate.py results JSON")
    parser.add_argument("--socket", required=True, help="Isolated daemon UNIX socket (required to avoid the normal live daemon)")
    parser.add_argument("--json", type=Path, help="Write aggregate and exact per-item results")
    args = parser.parse_args()
    if args.json and args.json.resolve() == args.input.resolve():
        parser.error("--json must not overwrite --input")
    baseline, input_hash = load_baseline(args.input)
    # Fail before sending anything if the normalizer dependency is unavailable.
    normalize("繁體 normalization")
    control = load_control()
    learned = control.request({"type": "list_learned"}, args.socket)
    if not isinstance(learned, list) or learned:
        raise ValueError("Evaluation requires an isolated empty learning store; no state was changed")
    run_id = uuid.uuid4().hex[:12]
    rows = []
    started = time.perf_counter()
    print("SAVED ASR TEXT ONLY — no audio decoding; references never enter daemon requests.", flush=True)
    print(f"{len(baseline)} paired cases; empty learning store verified. Use a fresh daemon without manual context.", flush=True)
    for position, item in enumerate(baseline, 1):
        reference_chars = to_chars(normalize(item["ref"]))
        row = {**item, "reference_chars": len(reference_chars),
               "raw_errors": errors(reference_chars, item["hyp"])}
        message = {
            "type": "process_text", "text": item["hyp"],
            "context_text": "", "selected_text": "", "program": "voicetype-postproc-eval",
            "context_id": f"postproc-eval:{run_id}:{item['set']}:{item['index']}",
        }
        try:
            for mode in ("off", "faithful"):
                start = time.perf_counter()
                output = control.request({**message, "mode": mode}, args.socket)
                elapsed_ms = (time.perf_counter() - start) * 1000
                if not isinstance(output, dict) or not isinstance(output.get("text"), str):
                    raise ValueError("process_text response must contain a text string")
                distance = errors(reference_chars, output["text"])
                row[mode] = {"text": output["text"], "errors": distance,
                             "cer": 100.0 * distance / max(len(reference_chars), 1),
                             "elapsed_ms": round(elapsed_ms, 3)}
            difference = row["faithful"]["errors"] - row["off"]["errors"]
            row["classification"] = "fix" if difference < 0 else "worse" if difference > 0 else "unchanged"
            row["candidate_false_correction"] = row["off"]["errors"] == 0 and row["faithful"]["errors"] > 0
            row["raw_correct_regressed"] = row["raw_errors"] == 0 and row["faithful"]["errors"] > 0
        except (OSError, RuntimeError, ValueError) as exc:
            row["classification"] = "error"
            row["error"] = str(exc)
        rows.append(row)
        print(f"[{position:03d}/{len(baseline):03d}] {item['set']}/{item['index']:03d}: "
              f"{row['classification']}", flush=True)
        report = report_for(rows, args, input_hash, run_id, len(baseline), time.perf_counter() - started)
        # Preserve completed pairs if a later request fails or the run stops.
        save_report(report, args.json)
        if row["classification"] == "error":
            print(row["error"], file=sys.stderr)
            return 2
    print(json.dumps(report["summary"], ensure_ascii=False, indent=2))
    for group in report["sets"]:
        print(f"{group['set']:<12} CER raw/off/faithful "
              f"{group['raw_cer']:.3f}/{group['off_cer']:.3f}/{group['faithful_cer']:.3f}% "
              f"fix={group['fix']} worse={group['worse']} unchanged={group['unchanged_error_count']}")
    if not report["summary"]["fix"]:
        print("No CER improvement demonstrated; unchanged output alone is not evidence that the model worked.")
    return 1 if report["summary"]["worse"] else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, RuntimeError) as exc:
        print(f"Postprocessing evaluation: {exc}", file=sys.stderr)
        sys.exit(2)
