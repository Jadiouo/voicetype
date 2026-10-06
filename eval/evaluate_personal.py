#!/usr/bin/env python3
"""Evaluate verified user-provided recordings without exposing answers to the ASR.

Example: .venv/bin/python eval/evaluate_personal.py --engine daemon \
  --engine-bin ~/.local/bin/voicetyped --model models/sense-voice-small-q8_0.gguf \
  --label sensevoice-current --json private/dictation-dev.json
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time
import unicodedata
import wave

import evaluate as scoring
from record_personal import DEFAULT_CORPUS, DEFAULT_OUTPUT, audio_quality, digest, load_corpus

TAG = re.compile(r"<\|([^|]*)\|>")
UNEXPECTED_SCRIPT = re.compile(r"[\u3040-\u30ff\u31f0-\u31ff\u3100-\u312f\u3130-\u318f\uac00-\ud7af]")
TIME_BIN = Path("/usr/bin/time")


def artifact_digest(path):
    """Hash large weights without loading another full model into evaluator RAM."""
    result = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def surface(text):
    """Term matching preserves punctuation; aliases explicitly permit variants."""
    if scoring._T2S is None:
        scoring._T2S = scoring._load_converter()
    return re.sub(r"\s+", " ", scoring._T2S.convert(unicodedata.normalize("NFKC", text)).casefold()).strip()


def term_count(text, term):
    variants = sorted({surface(t) for t in [term["text"], *term.get("aliases", [])]}, key=len, reverse=True)
    patterns = []
    for variant in variants:
        if not variant:
            raise ValueError("empty term/alias")
        left = r"(?<![a-z0-9_])" if re.match(r"[a-z0-9_]", variant[0]) else ""
        right = r"(?![a-z0-9_])" if re.match(r"[a-z0-9_]", variant[-1]) else ""
        patterns.append(left + re.escape(variant).replace(r"\ ", r"\s+") + right)
    # One alternation prevents an alias from counting the same occurrence twice.
    return sum(1 for _ in re.finditer("(?:" + "|".join(patterns) + ")", surface(text)))


def score_case(case, raw):
    text = TAG.sub("", raw).strip()
    reference = case["reference"]
    ref, hyp = scoring.normalize(reference), scoring.normalize(text)
    ref_chars, hyp_chars = scoring.to_chars(ref), scoring.to_chars(hyp)
    distance, substitutions, deletions, insertions = scoring.levenshtein(ref_chars, hyp_chars)
    terms = []
    for term in case.get("terms", []):
        expected, observed = term_count(reference, term), term_count(text, term)
        if expected == 0:
            raise ValueError(f"{case['id']}: target term absent from reference: {term['text']}")
        terms.append({"term": term["text"], "expected_occurrences": expected,
                      "observed_occurrences": observed, "matched_occurrences": min(expected, observed)})
    result = {"id": case["id"], "split": case["split"], "category": case["category"],
              "reference": reference, "raw_output": raw, "text": text,
              "reference_characters": len(ref_chars), "character_errors": distance,
              "substitutions": substitutions, "deletions": deletions, "insertions": insertions,
              "cer_percent": 100 * distance / len(ref_chars) if ref_chars else None,
              "normalized_exact": ref == hyp, "terms": terms,
              "unexpected_script_review": sorted(set(UNEXPECTED_SCRIPT.findall(text))),
              "unexpected_language_tags": sorted(set(TAG.findall(raw)) & {"ja", "ko", "yue"}),
              "pinyin_review": "manual_required",
              "numeric_representation_review": bool(re.search(r"[0-9零〇一二兩两三四五六七八九十百千萬万億亿]", reference + text))}
    if case["category"] == "english":
        words = ref.split()
        result["reference_words"] = len(words)
        result["word_errors"] = scoring.levenshtein(words, hyp.split())[0]
    return result


def aggregate(rows):
    chars = sum(r["reference_characters"] for r in rows)
    errors = sum(r["character_errors"] for r in rows)
    words = sum(r.get("reference_words", 0) for r in rows)
    word_errors = sum(r.get("word_errors", 0) for r in rows)
    terms = [t for r in rows for t in r["terms"]]
    expected = sum(t["expected_occurrences"] for t in terms)
    matched = sum(t["matched_occurrences"] for t in terms)
    exact = sum(r["normalized_exact"] for r in rows)
    return {"utterances": len(rows), "reference_characters": chars, "character_errors": errors,
            "cer_percent": 100 * errors / chars if chars else None,
            "english_reference_words": words, "english_word_errors": word_errors,
            "english_wer_percent": 100 * word_errors / words if words else None,
            "normalized_exact_utterances": exact,
            "normalized_exact_percent": 100 * exact / len(rows) if rows else None,
            "target_term_expected_occurrences": expected, "target_term_matched_occurrences": matched,
            "target_term_occurrence_recall_percent": 100 * matched / expected if expected else None,
            "unexpected_script_review_utterances": sum(bool(r["unexpected_script_review"]) for r in rows),
            "unexpected_language_tag_utterances": sum(bool(r["unexpected_language_tags"]) for r in rows)}


def validate_recordings(cases, corpus_path, recordings):
    recordings = Path(recordings)
    expected_hash = digest(corpus_path)
    errors, verified = [], {}
    frozen = recordings / "corpus.json"
    if not frozen.is_file() or digest(frozen) != expected_hash:
        errors.append({"id": None, "error": "missing or mismatched frozen recordings/corpus.json"})
    for case in cases:
        cid = case["id"]
        wav, metadata = recordings / f"{cid}.wav", recordings / f"{cid}.json"
        try:
            entry = json.loads(metadata.read_text())
            if not isinstance(entry, dict) or entry.get("id") != cid:
                raise ValueError("metadata id mismatch")
            if entry.get("confirmed_reading") is not True:
                raise ValueError("reading not explicitly confirmed")
            if entry.get("reference") != case["reference"]:
                raise ValueError("reference differs from frozen prompt; re-record or create a separately reviewed corpus")
            if entry.get("corpus_sha256") != expected_hash:
                raise ValueError("corpus hash mismatch")
            wav_hash = digest(wav)
            if entry.get("wav_sha256") != wav_hash:
                raise ValueError("recording hash mismatch")
            quality = audio_quality(wav)
            verified[cid] = {"wav": str(wav.resolve()), "wav_sha256": wav_hash, "quality": quality}
        except (OSError, ValueError, TypeError, wave.Error) as exc:
            errors.append({"id": cid, "error": str(exc)})
    return verified, errors


def engine_command(engine, binary, model, wav, threads=4, device="cpu", no_itn=False):
    """This interface deliberately accepts no corpus case or reference/context."""
    if device != "cpu":
        raise ValueError("AMD Vulkan adapter 尚未完成獨立 build／backend 驗證；此版拒絕執行，沒有退回 CPU 或 NVIDIA")
    if threads < 1:
        raise ValueError("threads must be positive")
    env = dict(os.environ)
    env.update(CUDA_VISIBLE_DEVICES="-1", HIP_VISIBLE_DEVICES="-1", ROCR_VISIBLE_DEVICES="-1",
               GGML_VK_VISIBLE_DEVICES="", VK_DRIVER_FILES="/dev/null")
    # Keep ambient context/refiner settings out of the isolated raw-ASR process.
    for key in list(env):
        if key.startswith("VOICETYPE_"):
            del env[key]
    if engine == "daemon":
        if threads != 4:
            raise ValueError("current daemon fixes inference threads at 4; other values are not supported")
        env["VOICETYPE_MODEL"] = str(model)
        cmd = [str(binary), "--transcribe", str(wav)]
        if no_itn:
            cmd.append("--no-itn")
    elif engine == "whisper":
        if no_itn:
            raise ValueError("--no-itn is a SenseVoice daemon option, not a Whisper option")
        cmd = [str(binary), "-ng", "-m", str(model), "-f", str(wav), "-t", str(threads), "-nt", "-l", "auto"]
    else:
        raise ValueError("unsupported engine")
    return cmd, env


def run_engine(cmd, env, timeout=300):
    with tempfile.TemporaryDirectory(prefix="voicetype-eval-") as tmp:
        rss_file = Path(tmp) / "peak-rss.txt"
        measured = [str(TIME_BIN), "-f", "%M", "-o", str(rss_file), *cmd] if TIME_BIN.is_file() else cmd
        start = time.perf_counter()
        proc = subprocess.Popen(measured, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                text=True, errors="replace", start_new_session=True)
        try:
            stdout, stderr = proc.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.communicate()
            raise RuntimeError(f"engine timed out after {timeout}s")
        elapsed = time.perf_counter() - start
        if proc.returncode:
            raise RuntimeError(f"engine exited {proc.returncode}: {stderr[-1000:]}")
        peak = None
        if rss_file.is_file():
            lines = rss_file.read_text().strip().splitlines()
            if lines and lines[-1].isdigit():
                peak = int(lines[-1])
        return {"raw_output": stdout.strip(), "elapsed_process_seconds": elapsed,
                "peak_rss_kib": peak, "stderr": stderr}


def report_notes():
    return [
        "Raw ASR only. No reference, target terms, notes, or input-box context are given to the engine.",
        "CER is pooled character edits / reference characters, including Latin characters; it is not pure Chinese CER.",
        "Normalization: NFKC, punctuation removal, lowercase, traditional-to-simplified; CER ignores whitespace. Exact match retains normalized word spacing.",
        "English WER is measured only for category=english. No WER claim is made for Chinese or mixed speech.",
        "Term occurrence recall counts canonical/alias occurrences, capped per term. It does not verify position, role, negation, or extra wrong names; inspect those manually.",
        "Unexpected-script/language flags need manual review. Latin output alone does not prove pinyin; pinyin counts are not automatically claimed.",
        "Numbers are NOT normalized for spoken/written equivalence. ITN may change CER without changing meaning; numeric cases are flagged for review.",
        "Timing launches one process per utterance and INCLUDES model load. It is NOT warm inference latency. Peak RSS is per child from GNU time, not cumulative RUSAGE_CHILDREN.",
        "Any missing, unconfirmed, tampered, or failed selected case makes the report incomplete, with no aggregate score.",
    ]


def evaluate(args):
    corpus = load_corpus(args.corpus)
    cases = [c for c in corpus["cases"] if args.split == "all" or c["split"] == args.split]
    report = {"schema_version": 1, "label": args.label, "status": "incomplete", "track": "raw_asr_no_context",
              "split": args.split, "expected_utterances": len(cases), "scored_utterances": 0,
              "corpus_sha256": digest(args.corpus), "engine": args.engine, "device": args.device,
              "threads": args.threads, "daemon_vad": args.engine == "daemon",
              "daemon_itn": (not args.no_itn) if args.engine == "daemon" else None,
              "aggregate": None, "categories": None, "cases": [], "errors": [], "notes": report_notes()}
    verified, errors = validate_recordings(cases, args.corpus, args.recordings)
    report["verified_recordings"] = len(verified)
    report["errors"].extend(errors)
    # Validate settings even when recordings are missing; never silently ignore device flags.
    try:
        engine_command(args.engine, args.engine_bin, args.model, "validation-only.wav", args.threads, args.device, args.no_itn)
        for name, path in [("engine_binary", args.engine_bin), ("model", args.model)]:
            report[name + "_path"] = str(path.resolve())
            report[name + "_sha256"] = artifact_digest(path)
        if not os.access(args.engine_bin, os.X_OK):
            raise ValueError("engine binary is not executable")
        scoring.normalize("正規化 preflight")
    except (OSError, ValueError, SystemExit) as exc:
        report["errors"].append({"id": None, "error": str(exc)})
    if report["errors"]:
        return report
    for case in cases:
        try:
            recording = verified[case["id"]]
            if digest(recording["wav"]) != recording["wav_sha256"]:
                raise ValueError("recording changed after validation")
            cmd, env = engine_command(args.engine, args.engine_bin, args.model, recording["wav"], args.threads, args.device, args.no_itn)
            output = run_engine(cmd, env, args.timeout)
            row = score_case(case, output.pop("raw_output"))
            row.update(output)
            row.update(recording)
            report["cases"].append(row)
            print(f"{case['id']} {len(report['cases'])}/{len(cases)}", file=sys.stderr, flush=True)
        except (OSError, ValueError, RuntimeError) as exc:
            report["errors"].append({"id": case["id"], "error": str(exc)})
    report["scored_utterances"] = len(report["cases"])
    if not report["errors"] and len(report["cases"]) == len(cases):
        report["status"] = "complete"
        report["aggregate"] = aggregate(report["cases"])
        report["categories"] = {category: aggregate([r for r in report["cases"] if r["category"] == category])
                                for category in sorted({c["category"] for c in cases})}
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", type=Path, default=DEFAULT_CORPUS)
    parser.add_argument("--recordings", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--engine", choices=["daemon", "whisper"], required=True)
    parser.add_argument("--engine-bin", type=Path, required=True)
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--label", required=True)
    parser.add_argument("--split", choices=["dev", "test", "all"], default="dev")
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--device", choices=["cpu", "amd-vulkan"], default="cpu")
    parser.add_argument("--no-itn", action="store_true", help="daemon only; keep spoken number representation")
    parser.add_argument("--timeout", type=float, default=300)
    parser.add_argument("--json", type=Path, required=True)
    args = parser.parse_args(argv)
    os.umask(0o077)
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    try:
        report = evaluate(args)
    except (OSError, ValueError, KeyError, TypeError) as exc:
        report = {"schema_version": 1, "status": "incomplete", "aggregate": None,
                  "errors": [{"id": None, "error": str(exc)}]}
    args.json.parent.mkdir(parents=True, exist_ok=True)
    args.json.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    args.json.chmod(0o600)
    print(f"{report['status']}: {args.json}")
    if report["status"] != "complete":
        print("評測未完成；沒有總成績。請檢查報告 errors。", file=sys.stderr)
        return 2
    print(f"CER={report['aggregate']['cer_percent']:.2f}%")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
