#!/usr/bin/env python3
"""CPU-only CSC diagnostic. No microphone, model network calls, or insertion.

Character offsets are applied to the original Traditional text. Non-Han tokens
are never reconstructed from decoded model tokens. This is an experiment,
not the production correction policy.
"""
import argparse
import json
import math
from pathlib import Path
import statistics
import sys
import time

import numpy as np
import onnxruntime as ort
from opencc import OpenCC
from tokenizers import Tokenizer

DEV = [
    ("今天新情很好。", "今天心情很好。"),
    ("這次測試的結過還不錯。", "這次測試的結果還不錯。"),
    ("我們先把資料備份，在更新應用程式。", "我們先把資料備份，再更新應用程式。"),
    ("記得檢查檔案的路境。", "記得檢查檔案的路徑。"),
    ("把這個功能部屬到伺服器。", "把這個功能部署到伺服器。"),
    ("我希望這個系統可以穩定運做。", "我希望這個系統可以穩定運作。"),
    ("如果發生意外就立既停止。", "如果發生意外就立即停止。"),
    ("這個問題己經修正了。", "這個問題已經修正了。"),
    ("這個設定因該可以使用。", "這個設定應該可以使用。"),
    ("今天心情很好。", None),
    ("這次測試的結果還不錯。", None),
    ("我們在更新應用程式，請稍等。", None),
    ("他是我的部屬，不是主管。", None),
    ("我自己寫的程式沒有問題。", None),
    ("我不希望刪除這個 commit，也不要把 0.05 改成 0.5。", None),
    ("請保留 GitHub、VoiceType 和 ExampleApp。", None),
    ("請檢查說明文件中的中文段落和英文標題。", None),
    ("今天下午去公園散步，回家後整理書架。", None),
    ("陳小明和李小華今天都沒有到。", None),
    ("變數名稱是 `路境`，不要改它的拼法。", None),
    ("檔案在 /tmp/路境.txt，這個名稱是刻意取的。", None),
    ("我在臺灣用軟體與記憶體，資料存在硬碟。", None),
    ("嗯，我、我想先試試看，不對，還是明天再說。", None),
    ("Use catch to handle errors; the human brain is complex.", None),
]


def han(ch):
    return len(ch) == 1 and "\u4e00" <= ch <= "\u9fff"


class Predictor:
    def __init__(self, model, tokenizer, threads=4):
        options = ort.SessionOptions()
        options.intra_op_num_threads = threads
        options.inter_op_num_threads = 1
        options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
        self.session = ort.InferenceSession(str(model), sess_options=options,
                                            providers=["CPUExecutionProvider"])
        assert self.session.get_providers() == ["CPUExecutionProvider"]
        self.tokenizer = Tokenizer.from_file(str(tokenizer))
        self.to_simple, self.to_trad = OpenCC("t2s"), OpenCC("s2tw")

    def predict(self, text, minimum=0.9):
        simple = self.to_simple.convert(text)
        if len(simple) != len(text):
            return text, [], "alignment_skip"
        encoded = self.tokenizer.encode(simple)
        if len(encoded.ids) > 256:
            return text, [], "length_skip"
        arrays = dict(input_ids=encoded.ids, attention_mask=encoded.attention_mask,
                      token_type_ids=encoded.type_ids)
        inputs = {x.name: np.asarray([arrays[x.name]], dtype=np.int64)
                  for x in self.session.get_inputs()}
        logits = self.session.run(None, inputs)[0][0]
        output = list(text)
        edits = []
        for i, (start, end) in enumerate(encoded.offsets):
            if end - start != 1 or not han(simple[start:end]):
                continue
            winner = int(np.argmax(logits[i]))
            target = self.tokenizer.id_to_token(winner)
            if not han(target) or target == simple[start:end]:
                continue
            shifted = logits[i] - float(logits[i].max())
            normalizer = float(np.exp(shifted).sum())
            probability = float(np.exp(shifted[winner])) / normalizer
            original_probability = float(np.exp(shifted[encoded.ids[i]])) / normalizer
            corrected = self.to_trad.convert(target)
            accepted = probability >= minimum and len(corrected) == 1
            edits.append(dict(start=start, source=text[start:end], target=corrected,
                              probability=probability, original_probability=original_probability,
                              accepted=accepted))
            if accepted:
                output[start] = corrected
        return "".join(output), edits, "called"


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--model", type=Path, required=True)
    ap.add_argument("--tokenizer", type=Path, required=True)
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--threshold", type=float, default=0.9)
    ap.add_argument("--threads", type=int, default=4)
    ap.add_argument("--guarded", action="store_true")
    args = ap.parse_args()
    started = time.perf_counter()
    if args.guarded:
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]/"scripts"))
        from voicetype_csc import Engine, apply_edits
        model = Engine(args.model, args.tokenizer, args.threads)
        model.predict("今天心情很好。", budget_ms=1000)  # Service startup warmup.
    else:
        model = Predictor(args.model, args.tokenizer, args.threads)
    load_ms = (time.perf_counter() - started) * 1000
    rows = []
    for repetition in range(2):
        for text, expected in DEV:
            expected = text if expected is None else expected
            started = time.perf_counter()
            if args.guarded:
                edits, status = model.predict(text)
                actual = apply_edits(text, edits)
            else:
                actual, edits, status = model.predict(text, args.threshold)
            rows.append(dict(text=text, expected=expected, actual=actual, edits=edits,
                             elapsed_ms=(time.perf_counter()-started)*1000,
                             repetition=repetition, status=status, correct=actual == expected))
    warm = sorted(r["elapsed_ms"] for r in rows[len(DEV):] if r["status"] in ["called", "applied", "unchanged"])
    summary = dict(model=str(args.model), providers=model.session.get_providers(), load_ms=load_ms,
                   first_ms=rows[0]["elapsed_ms"], warm_median_ms=statistics.median(warm),
                   warm_p95_ms=warm[math.ceil(len(warm)*.95)-1], threads=args.threads,
                   correct=sum(r["correct"] for r in rows), total=len(rows))
    args.output.write_text(json.dumps(dict(summary=summary, cases=rows), ensure_ascii=False, indent=2)+"\n")
    print(json.dumps(summary, ensure_ascii=False))
    for row in rows[:len(DEV)]:
        print(json.dumps({k:row[k] for k in ["text","actual","edits","elapsed_ms"]},ensure_ascii=False))


if __name__ == "__main__":
    main()
