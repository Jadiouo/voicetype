#!/usr/bin/env python3
"""ASR 評測: CER / WER 與 R1 決策 (SDD §8.1)。

對每個錄音跑 ASR, 與參考文本比對, 輸出各集合的 CER/WER 與整體報告。

**這支工具存在的唯一理由是回答一個問題**: SenseVoice 的中英夾雜品質
可不可用? SDD §10/R1 把它列為最高風險, §8.1 訂了明確的決策點 ——
`mixed` 集合 CER > 20% 就要啟動備案 (per-app 切換引擎)。報告最後會
直接給出這個判斷。

用法:
    python3 eval/evaluate.py                      # 全部集合
    python3 eval/evaluate.py --set mixed          # 只跑一組
    python3 eval/evaluate.py --json out.json      # 另存機器可讀結果
"""

import argparse
import json
import os
import re
import subprocess
import sys
import unicodedata

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CORPUS_DIR = os.path.join(ROOT, "eval", "corpus")
REC_DIR = os.path.join(ROOT, "eval", "recordings")

SETS = ["zh_pure", "en_pure", "mixed", "terminal", "filler"]

DEFAULT_ENGINE = os.path.join(
    ROOT, "third_party", "SenseVoice.cpp", "build", "bin", "sense-voice-main"
)
DEFAULT_MODEL = os.path.join(ROOT, "models", "sense-voice-small-q8_0.gguf")

# SDD §1.3 的成功標準。zh_pure 看 CER, en_pure 看 WER。
CER_TARGETS = {"zh_pure": 12.0}
WER_TARGETS = {"en_pure": 10.0}

# WER 只對以英文為主的集合有意義。中文沒有詞邊界, 按空白切詞會把
# 整句算成一個「詞」, 錯一個字就是 100% WER —— 那是雜訊不是指標。
WER_MEANINGFUL = {"en_pure", "terminal"}

# SDD §8.1 的決策點
MIXED_DECISION_THRESHOLD = 20.0


# --------------------------------------------------------------------------
# 正規化
#
# 比對前必須把兩邊拉到同一個基準, 否則量到的是格式差異而不是辨識錯誤。
# 做四件事:
#   1. 移除標點 (標點策略由 profile 決定, 不是辨識能力)
#   2. 英文轉小寫 (大小寫同樣是後處理職責)
#   3. 全形轉半形 + 收斂空白
#   4. 繁簡摺疊: 兩邊都轉簡體
#
# 關於第 4 點 —— 這是本工具最初版本的一個嚴重缺陷, 值得留下記錄:
#
# 最初刻意不做繁簡轉換, 理由是「否則永遠不會發現引擎輸出的是簡體」。
# 那個理由是錯的: 引擎輸出簡體在第一次跑的時候就看到了, 而保留這個
# 行為的實際後果是, 一句**完全辨識正確**的中文
#
#     參考 明天下午三點我們開會討論這件事情
#     辨識 明天下午三点我们开会讨论这件事情
#
# 被算成 43.8% CER。整組 zh_pure 因此報出 36.2%, 而 SenseVoice 的實際
# 中文 CER 應該在 7.8–10.8%。差距全部來自繁簡, 不是辨識錯誤。
#
# 教訓: 繁化是後處理管線 §4.6 ② 的職責, 屬於管線的另一段。把它的
# 缺件算進引擎的分數, 等於用錯的數字去做 R1 這種架構級決策。
#
# 轉換方向選繁→簡 (t2s) 而非簡→繁: t2s 是多對一的確定性映射, 不會
# 引入歧義; s2t 有一簡對多繁的情況, 轉換本身就會產生誤差, 那個誤差
# 會被誤算成辨識錯誤。
#
# 繁化的正確性是**另一個**測試項 (SDD §8.2: 後處理是純函式, 容易測,
# 覆蓋率應該高), 與 ASR 準確度分開量。
# --------------------------------------------------------------------------

PUNCT_RE = re.compile(
    r"[，。、；：？！「」『』（）《》〈〉…—～·"
    r",.;:?!\"'()\[\]{}<>/\\|`~@#$%^&*_+=\-]"
)


def _load_converter():
    """繁→簡轉換器。

    刻意在匯入時就硬失敗而不是靜默跳過 —— 少了這一步產生的數字看起來
    完全合理 (沒有例外、沒有警告、格式正確), 卻會讓 R1 決策整個做反。
    寧可跑不起來。
    """
    try:
        from opencc import OpenCC
    except ImportError:
        sys.exit(
            "找不到 opencc 模組, 無法做繁簡正規化。\n"
            "\n"
            "少了這一步, 繁體參考文本與簡體辨識結果的每一個字都會被算成\n"
            "錯誤, CER 會虛高到失去意義 —— 這足以讓 R1 決策做反。\n"
            "\n"
            "安裝:\n"
            "    python3 -m venv .venv\n"
            "    .venv/bin/pip install opencc-python-reimplemented\n"
            "    .venv/bin/python eval/evaluate.py\n"
        )
    return OpenCC("t2s")


_T2S = None


def normalize(text):
    global _T2S
    if _T2S is None:
        _T2S = _load_converter()
    text = unicodedata.normalize("NFKC", text)
    text = PUNCT_RE.sub("", text)
    text = text.lower()
    text = _T2S.convert(text)
    text = re.sub(r"\s+", " ", text).strip()
    return text


def to_chars(text):
    """字元序列。CJK 逐字, 拉丁字母保持成詞內字元, 空白不計入。"""
    return [c for c in text if not c.isspace()]


def to_words(text):
    return text.split()


def levenshtein(ref, hyp):
    """回傳 (距離, 替換, 刪除, 插入)。"""
    n, m = len(ref), len(hyp)
    if n == 0:
        return m, 0, 0, m
    if m == 0:
        return n, 0, n, 0

    # (cost, sub, del, ins)
    prev = [(j, 0, 0, j) for j in range(m + 1)]
    for i in range(1, n + 1):
        cur = [(i, 0, i, 0)] + [None] * m
        for j in range(1, m + 1):
            if ref[i - 1] == hyp[j - 1]:
                cur[j] = prev[j - 1]
            else:
                sub = prev[j - 1]
                dele = prev[j]
                ins = cur[j - 1]
                best = min(sub[0], dele[0], ins[0])
                if best == sub[0]:
                    cur[j] = (sub[0] + 1, sub[1] + 1, sub[2], sub[3])
                elif best == dele[0]:
                    cur[j] = (dele[0] + 1, dele[1], dele[2] + 1, dele[3])
                else:
                    cur[j] = (ins[0] + 1, ins[1], ins[2], ins[3] + 1)
        prev = cur
    return prev[m]


# --------------------------------------------------------------------------
# 引擎
# --------------------------------------------------------------------------

# sense-voice-main 每段輸出形如 "[0.96-5.18] 文字內容"
SEGMENT_RE = re.compile(r"^\[\s*[\d.]+\s*-\s*[\d.]+\s*\]\s*(.*)$")
# SenseVoice 的結構化標籤 (SDD §4.6 ①)
TAG_RE = re.compile(r"<\|[^|]*\|>")


def run_sensevoice(engine, model, wav, threads=4):
    proc = subprocess.run(
        [engine, "-m", model, "-f", wav, "-t", str(threads)],
        capture_output=True,
        text=True,
        timeout=120,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"engine failed: {proc.stderr.strip()[-300:]}")

    segments = []
    for line in proc.stdout.splitlines():
        m = SEGMENT_RE.match(line.strip())
        if m:
            segments.append(m.group(1))
    text = " ".join(segments)
    return TAG_RE.sub("", text).strip()


# 由 --prompt 設定。whisper 支援 initial prompt, SenseVoice 不支援
# (見 docs/sdd-deviations.md D7) —— 這正是 SDD §10 未解問題 #1 想要的
# 「在推論階段注入詞彙」, 對 terminal 情境可能是決定性的差異。
WHISPER_PROMPT = None


def run_whisper(engine, model, wav, threads=4):
    """SDD §8.1 指定的 baseline: whisper small int8 CPU。

    `-nt` 關掉時間戳, 輸出就是純文字。語言留給模型自動判定 —— 這正是
    要跟 SenseVoice 比較的能力, 先驗地指定語言等於偷跑。
    """
    cmd = [engine, "-m", model, "-f", wav, "-t", str(threads), "-nt", "-l", "auto"]
    if WHISPER_PROMPT:
        cmd += ["--prompt", WHISPER_PROMPT]
    proc = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        timeout=300,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"engine failed: {proc.stderr.strip()[-300:]}")
    # whisper-cli 把轉錄寫到 stdout, 進度與統計走 stderr。
    return " ".join(line.strip() for line in proc.stdout.splitlines()).strip()


def _run_daemon(engine, model, wav, threads, use_vad, use_itn=False):
    """voicetyped 自己的推論管線 (`--transcribe`)。

    存在的理由是評測的可信度。`sensevoice` runner 量的是上游的
    sense-voice-main CLI, 但產品跑的是 `voicetyped/shim` —— CTC 去重的
    實作、ITN 設定、M1 起還多了 VAD 修剪, 都可能讓兩者輸出不同的文字。
    拿 CLI 的分數當產品的分數是類別錯誤。

    `daemon-novad` 是同一條管線關掉 VAD, 用來回答「修剪有沒有吃掉
    第一個音節」—— 那是接上 VAD 的主要風險 (SDD §4.4 的 pre-roll 花了
    500ms 緩衝在防同一件事)。

    **ITN 預設關掉**, 與繁簡摺疊同一個理由。產品實際是開著的 (聽寫想要
    「32」而不是「三十二」), 但語料的參考文本寫的是中文數字:

        參考  明天下午三點我們開會討論這件事情
        辨識  明天下午3点，我们开会讨论这件事。

    ITN 開著時這句報 12.5% CER, 而它一個字都沒聽錯。那是表示差異不是
    辨識錯誤, 算進分數就會像當初繁簡那樣污染整組數字 (zh_pure 4.9 →
    6.2%)。要看產品的實際輸出用 `daemon-itn`, 但別拿它的分數跟其他
    engine-type 比。
    """
    cmd = [engine, "--transcribe", wav]
    if not use_vad:
        cmd.append("--no-vad")
    if not use_itn:
        cmd.append("--no-itn")
    env = dict(os.environ, VOICETYPE_MODEL=model)
    proc = subprocess.run(
        cmd, capture_output=True, text=True, timeout=120, env=env
    )
    if proc.returncode != 0:
        raise RuntimeError(f"engine failed: {proc.stderr.strip()[-300:]}")
    text = " ".join(line.strip() for line in proc.stdout.splitlines())
    return TAG_RE.sub("", text).strip()


ENGINES = {
    "sensevoice": run_sensevoice,
    "whisper": run_whisper,
    "daemon": lambda e, m, w, t=4: _run_daemon(e, m, w, t, use_vad=True),
    "daemon-novad": lambda e, m, w, t=4: _run_daemon(e, m, w, t, use_vad=False),
    # 產品的實際設定 (ITN 開)。分數不可與其他 engine-type 比較 —— 見上。
    "daemon-itn": lambda e, m, w, t=4: _run_daemon(e, m, w, t, use_vad=True, use_itn=True),
}

# `daemon*` 用的是 daemon 執行檔而不是上游 CLI。
DAEMON_BIN = os.path.join(ROOT, "voicetyped", "target", "release", "voicetyped")


def load_corpus(name):
    path = os.path.join(CORPUS_DIR, f"{name}.txt")
    out = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line and not line.startswith("#"):
                out.append(line)
    return out


# --------------------------------------------------------------------------

def evaluate_set(name, runner, engine, model, threads, verbose):
    sentences = load_corpus(name)
    outdir = os.path.join(REC_DIR, name)

    ref_chars = hyp_errors = 0
    ref_words = word_errors = 0
    items = []
    missing = 0

    for idx, ref in enumerate(sentences, start=1):
        wav = os.path.join(outdir, f"{idx:03d}.wav")
        if not os.path.exists(wav):
            missing += 1
            continue

        try:
            hyp = runner(engine, model, wav, threads)
        except Exception as e:
            print(f"  [{idx:03d}] 引擎錯誤: {e}", file=sys.stderr)
            continue

        nref, nhyp = normalize(ref), normalize(hyp)
        rc, hc = to_chars(nref), to_chars(nhyp)
        dist, *_ = levenshtein(rc, hc)
        ref_chars += len(rc)
        hyp_errors += dist

        rw, hw = to_words(nref), to_words(nhyp)
        wdist, *_ = levenshtein(rw, hw)
        ref_words += len(rw)
        word_errors += wdist

        cer = 100.0 * dist / max(len(rc), 1)
        items.append({"index": idx, "ref": ref, "hyp": hyp, "cer": cer})

        if verbose:
            mark = "  " if cer < 10 else ("~ " if cer < 25 else "! ")
            print(f"  {mark}[{idx:03d}] CER {cer:5.1f}%")
            print(f"       參考: {ref}")
            print(f"       辨識: {hyp}")

    return {
        "set": name,
        "evaluated": len(items),
        "missing": missing,
        "total": len(sentences),
        "cer": 100.0 * hyp_errors / ref_chars if ref_chars else None,
        "wer": 100.0 * word_errors / ref_words if ref_words else None,
        "items": items,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", dest="sets", action="append", choices=SETS)
    ap.add_argument("--engine", help="引擎執行檔 (預設隨 --engine-type 而定)")
    ap.add_argument("--engine-type", default="sensevoice", choices=sorted(ENGINES))
    ap.add_argument("--label", help="報告中顯示的引擎名稱")
    ap.add_argument("--prompt", help="whisper initial prompt (注入技術詞彙)")
    ap.add_argument("--model", default=DEFAULT_MODEL)
    ap.add_argument("--threads", type=int, default=4)
    ap.add_argument("--verbose", "-v", action="store_true", help="逐句顯示")
    ap.add_argument("--json", metavar="PATH", help="另存 JSON 結果")
    args = ap.parse_args()

    if not args.engine:
        args.engine = (
            DAEMON_BIN if args.engine_type.startswith("daemon") else DEFAULT_ENGINE
        )

    for path, what in ((args.engine, "引擎"), (args.model, "模型")):
        if not os.path.exists(path):
            if path == DAEMON_BIN:
                sys.exit(
                    f"找不到 daemon: {path}\n\n先建置:\n  "
                    "cd voicetyped && cargo build --release --features sensevoice"
                )
            sys.exit(f"找不到{what}: {path}")

    if args.prompt:
        if args.engine_type != "whisper":
            sys.exit("--prompt 只有 whisper 支援 (見 docs/sdd-deviations.md D7)")
        globals()["WHISPER_PROMPT"] = args.prompt
        print(f"initial prompt: {args.prompt!r}")

    targets = args.sets or SETS
    results = []

    for name in targets:
        outdir = os.path.join(REC_DIR, name)
        if not os.path.isdir(outdir) or not os.listdir(outdir):
            print(f"[{name}] 尚未錄音, 跳過。先跑: python3 eval/record.py {name}")
            continue
        print(f"\n[{name}]")
        r = evaluate_set(name, ENGINES[args.engine_type], args.engine, args.model,
                         args.threads, args.verbose)
        results.append(r)
        if r["evaluated"]:
            wer = f"   WER {r['wer']:.1f}%" if name in WER_MEANINGFUL else ""
            print(f"  CER {r['cer']:.1f}%{wer}   "
                  f"({r['evaluated']}/{r['total']} 句)")
        if r["missing"]:
            print(f"  註: {r['missing']} 句尚未錄製")

    if not results:
        print("\n沒有可評測的錄音。先用 eval/record.py 錄製語料。")
        return 1

    # ---------------- 報告 ----------------
    print("\n" + "=" * 58)
    print(f"{'集合':<12} {'CER':>8} {'WER':>8} {'句數':>8}  {'目標':>10}")
    print("-" * 58)
    for r in results:
        if not r["evaluated"]:
            continue
        name = r["set"]
        wstr = f"{r['wer']:>7.1f}%" if name in WER_MEANINGFUL else f"{'—':>8}"

        if name in CER_TARGETS:
            target = CER_TARGETS[name]
            tstr, ok = f"CER <{target:.0f}%", r["cer"] < target
        elif name in WER_TARGETS:
            target = WER_TARGETS[name]
            tstr, ok = f"WER <{target:.0f}%", r["wer"] < target
        else:
            tstr, ok = "—", None
        verdict = "" if ok is None else (" ✓" if ok else " ✗")

        print(f"{name:<12} {r['cer']:>7.1f}% {wstr} "
              f"{r['evaluated']:>8}  {tstr:>10}{verdict}")
    print("=" * 58)
    print("註: WER 只對以英文為主的集合有意義 (中文無詞邊界)。")

    # ---------------- R1 決策 (SDD §8.1) ----------------
    mixed = next((r for r in results if r["set"] == "mixed" and r["evaluated"]), None)
    if mixed:
        print("\nR1 決策點 (SDD §8.1 / §10)")
        print(f"  mixed 集合 CER = {mixed['cer']:.1f}%   門檻 = {MIXED_DECISION_THRESHOLD:.0f}%")
        if mixed["cer"] > MIXED_DECISION_THRESHOLD:
            print("\n  → 超過門檻。依 SDD §8.1 應啟動備案:")
            print("     per-app 切換引擎 —— 中文情境用 SenseVoice,")
            print("     程式情境用 whisper small (同樣 CPU / ~250MB)。")
            print("     架構上需要多一層引擎抽象 (asr::Transcriber 已預留)。")
        else:
            print("\n  → 未超過門檻。維持 SDD §4.1 的單一引擎設計,")
            print("     中英夾雜的殘餘錯誤交給 §4.6 ③ 詞彙修正表處理。")
    else:
        print("\n注意: mixed 集合尚未錄製 —— R1 這個最高風險項還沒被驗證。")
        print("      SDD §8.1: 「這必須是第一個做的事, 不是最後一個。」")

    if args.json:
        with open(args.json, "w", encoding="utf-8") as f:
            json.dump(results, f, ensure_ascii=False, indent=2)
        print(f"\n已寫入 {args.json}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
