#!/usr/bin/env python3
"""語料錄製工具 (SDD §8.1)。

逐句提示、錄音、存成 16kHz mono WAV —— 也就是 SenseVoice 的輸入格式
(SDD §4.4), 錄製階段就對齊可以避免評測時多一層重採樣造成的變異。

錄好的檔案放在 eval/recordings/<集合名>/<編號>.wav。已存在的會被跳過,
所以可以分次錄完 125 句。

用法:
    python3 eval/record.py mixed            # 錄 mixed 集合
    python3 eval/record.py mixed --redo 7   # 重錄第 7 句
    python3 eval/record.py --list           # 看進度
"""

import argparse
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CORPUS_DIR = os.path.join(ROOT, "eval", "corpus")
REC_DIR = os.path.join(ROOT, "eval", "recordings")

SETS = ["zh_pure", "en_pure", "mixed", "terminal", "filler"]


def load_corpus(name):
    path = os.path.join(CORPUS_DIR, f"{name}.txt")
    if not os.path.exists(path):
        sys.exit(f"找不到語料檔: {path}")
    lines = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line and not line.startswith("#"):
                lines.append(line)
    return lines


def record_one(path, device=None):
    """錄一句。按 Enter 開始, 再按 Enter 結束。"""
    cmd = ["arecord", "-q", "-f", "S16_LE", "-r", "16000", "-c", "1"]
    if device:
        cmd += ["-D", device]
    cmd.append(path)

    proc = subprocess.Popen(cmd, stderr=subprocess.PIPE)
    try:
        input()
    except (KeyboardInterrupt, EOFError):
        proc.terminate()
        proc.wait()
        if os.path.exists(path):
            os.remove(path)
        raise
    proc.terminate()
    proc.wait()

    if not os.path.exists(path) or os.path.getsize(path) < 1000:
        err = proc.stderr.read().decode(errors="replace") if proc.stderr else ""
        print(f"  警告: 錄音檔異常小或不存在。{err.strip()}")
        return False
    return True


def show_progress():
    print(f"{'集合':<12} {'已錄':>6} / {'總數':>4}")
    print("-" * 28)
    total_done = total_all = 0
    for name in SETS:
        sentences = load_corpus(name)
        outdir = os.path.join(REC_DIR, name)
        done = 0
        if os.path.isdir(outdir):
            done = len([f for f in os.listdir(outdir) if f.endswith(".wav")])
        total_done += done
        total_all += len(sentences)
        print(f"{name:<12} {done:>6} / {len(sentences):>4}")
    print("-" * 28)
    print(f"{'總計':<12} {total_done:>6} / {total_all:>4}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("set_name", nargs="?", choices=SETS)
    ap.add_argument("--device", help="ALSA 裝置 (預設用系統預設輸入)")
    ap.add_argument("--redo", type=int, metavar="N", help="重錄第 N 句 (從 1 起算)")
    ap.add_argument("--list", action="store_true", help="顯示各集合的錄製進度")
    args = ap.parse_args()

    if args.list or not args.set_name:
        show_progress()
        if not args.set_name:
            print("\n指定集合名稱開始錄製, 例如: python3 eval/record.py mixed")
        return 0

    sentences = load_corpus(args.set_name)
    outdir = os.path.join(REC_DIR, args.set_name)
    os.makedirs(outdir, exist_ok=True)

    todo = list(enumerate(sentences, start=1))
    if args.redo:
        if not 1 <= args.redo <= len(sentences):
            sys.exit(f"--redo 必須在 1..{len(sentences)} 之間")
        todo = [(args.redo, sentences[args.redo - 1])]

    print(f"集合: {args.set_name}  ({len(sentences)} 句)")
    print("按 Enter 開始錄音, 說完再按 Enter 結束。Ctrl-C 離開。")
    print("錄壞了沒關係 —— 之後用 --redo N 重錄該句即可。\n")

    recorded = 0
    try:
        for idx, text in todo:
            path = os.path.join(outdir, f"{idx:03d}.wav")
            if os.path.exists(path) and not args.redo:
                continue

            print(f"[{idx:>3}/{len(sentences)}] {text}")
            input("        準備好按 Enter 開始 ... ")
            print("        ● 錄音中 (說完按 Enter)", end="", flush=True)
            ok = record_one(path, args.device)
            print("        ✓ 已存檔\n" if ok else "        ✗ 失敗\n")
            if ok:
                recorded += 1
    except (KeyboardInterrupt, EOFError):
        print("\n\n中斷。已錄的檔案保留, 下次執行會從沒錄的地方繼續。")

    print(f"\n本次錄了 {recorded} 句。")
    show_progress()
    return 0


if __name__ == "__main__":
    sys.exit(main())
