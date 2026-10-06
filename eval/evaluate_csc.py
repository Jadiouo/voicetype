#!/usr/bin/env python3
"""Replay frozen existing ASR text through a CPU spelling model; no audio.

References and target terms are only read by the scorer after prediction. The
input baseline is the running daemon's dictionary/learning output in mode off.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import statistics
import sys
import time

from evaluate_personal import score_case, aggregate


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--source", required=True, type=Path)
    ap.add_argument("--corpus", default=Path(__file__).parent / "fixtures/dictation-demo.json", type=Path)
    ap.add_argument("--split", choices=["dev", "test"], required=True)
    ap.add_argument("--model", type=Path)
    ap.add_argument("--tokenizer", type=Path)
    ap.add_argument("--daemon-socket", help="Compare default/off mode in the same actual text pipeline")
    ap.add_argument("--output", required=True, type=Path)
    ap.add_argument("--guarded", action="store_true")
    args = ap.parse_args()
    if not args.daemon_socket and (not args.model or not args.tokenizer):
        ap.error("supply --daemon-socket or both --model and --tokenizer")
    spec = importlib.util.spec_from_file_location("control", Path(__file__).resolve().parents[1]/"scripts/voicetype-control.py")
    control = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(control)
    cases = [r for r in json.loads(args.source.read_text())["cases"] if r["split"] == args.split]
    corpus = {r["id"]:r for r in json.loads(args.corpus.read_text())["cases"]}
    if args.daemon_socket:
        model = None
    elif args.guarded:
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]/"scripts"))
        from voicetype_csc import Engine, apply_edits
        model = Engine(args.model, args.tokenizer)
        model.predict("今天心情很好。", budget_ms=1000)
    else:
        from benchmark_csc import Predictor
        model = Predictor(args.model, args.tokenizer)
    rows, before_scores, after_scores = [], [], []
    for case in cases:
        text = control.request(dict(type="process_text", text=case["raw_output"], mode="off"), args.daemon_socket)["text"]
        start = time.perf_counter()
        if args.daemon_socket:
            out = control.request(dict(type="process_text", text=case["raw_output"]), args.daemon_socket)["text"]
            edits, status = None, "daemon_pipeline"
        elif args.guarded:
            edits, status = model.predict(text)
            out = apply_edits(text, edits)
        else:
            out, edits, status = model.predict(text)
        elapsed = (time.perf_counter()-start)*1000
        reference = corpus[case["id"]]
        assert reference["reference"] == case["reference"]
        before, after = score_case(reference, text), score_case(reference, out)
        before_scores.append(before)
        after_scores.append(after)
        rows.append(dict(id=case["id"], text=text, actual=out, reference=case["reference"],
                         edits=edits, status=status, elapsed_ms=elapsed,
                         errors_before=before["character_errors"], errors_after=after["character_errors"]))
    durations = sorted(r["elapsed_ms"] for r in rows)
    report = dict(source=str(args.source), model=str(args.model), split=args.split, daemon_socket=args.daemon_socket,
                  scope="Frozen existing ASR text; not new ASR inference; no references fed to model",
                  base=aggregate(before_scores), after=aggregate(after_scores), cases=rows,
                  median_ms=statistics.median(durations), p95_ms=durations[min(len(durations)-1,int(.95*len(durations)))], max_ms=max(durations))
    with args.output.open("x") as stream:
        stream.write(json.dumps(report,ensure_ascii=False,indent=2)+"\n")
    args.output.chmod(0o600)
    print(json.dumps({k:v for k,v in report.items() if k != "cases"}, ensure_ascii=False))
    for row in rows:
        if row["text"] != row["actual"]:
            print(json.dumps(row,ensure_ascii=False))


if __name__ == "__main__":
    main()
