#!/usr/bin/env python3
"""量測 daemon 的資源佔用 (SDD §6.1 / §6.2)。

回答三個問題:

  1. 閒置時佔多少 RAM / CPU? (§1.3 的「常駐記憶體 < 400MB、閒置 CPU 0%」)
  2. 錄音時多多少?
  3. 推論那一瞬間的尖峰是多少?

方法是對 daemon 的 PID 做高頻取樣 (預設 20Hz), 同時透過 IPC 驅動一次
完整的錄音→推論循環。**不**用 `systemctl status` 的單次數字 —— 推論
只有 200ms, 單次取樣看到尖峰的機率很低。

用法:
    python3 scripts/measure-resources.py            # 錄 3 秒
    python3 scripts/measure-resources.py --hold 5
    python3 scripts/measure-resources.py --json out.json

注意這會**開啟麥克風錄音**, 錄到的音訊只留在 daemon 的記憶體裡
(不寫檔), 但指示燈會亮。
"""

import argparse
import json
import os
import socket
import sys
import threading
import time

CLK_TCK = os.sysconf("SC_CLK_TCK")
PAGE_SIZE = os.sysconf("SC_PAGE_SIZE")


def socket_path():
    runtime = os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"
    return os.environ.get("VOICETYPE_SOCKET") or f"{runtime}/voicetype/ipc.sock"


def find_daemon():
    """找出正在跑的 voicetyped。

    比對 /proc/*/comm 而不是掃 cmdline: cmdline 會同時命中這支腳本
    自己 (argv 裡有 'voicetyped' 這個字), 而 comm 是執行檔名。
    """
    found = []
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(f"/proc/{entry}/comm") as f:
                if f.read().strip() == "voicetyped":
                    found.append(int(entry))
        except OSError:
            continue
    return found


class Sampler(threading.Thread):
    """對 /proc/<pid> 取樣 RSS 與 CPU 時間。

    CPU 用 utime+stime 的**差分**除以牆鐘時間, 而不是讀 /proc/<pid>/stat
    的即時欄位 —— 後者沒有即時 CPU%, 只有累計時間。差分才能看出
    「推論那 200ms 用了幾個核心」。
    """

    def __init__(self, pid, interval):
        super().__init__(daemon=True)
        self.pid = pid
        self.interval = interval
        self.samples = []
        self._done = threading.Event()

    def _read(self):
        with open(f"/proc/{self.pid}/stat") as f:
            line = f.read()
        # 第二個欄位是 comm, 被括號包住且**可以含空白與括號**。
        # 直接 split() 會錯位, 所以從最後一個 ')' 之後開始數:
        # rest[0] 是 state (欄位 3), 於是 utime/stime (欄位 14/15)
        # 落在 rest[11] 與 rest[12]。
        rest = line[line.rindex(")") + 2 :].split()
        utime, stime = int(rest[11]), int(rest[12])
        with open(f"/proc/{self.pid}/statm") as f:
            rss_pages = int(f.read().split()[1])
        return (utime + stime) / CLK_TCK, rss_pages * PAGE_SIZE

    def run(self):
        prev_cpu, prev_t = None, None
        while not self._done.is_set():
            now = time.monotonic()
            try:
                cpu, rss = self._read()
            except (OSError, ValueError, IndexError):
                break
            if prev_cpu is not None:
                dt = now - prev_t
                cores = (cpu - prev_cpu) / dt if dt > 0 else 0.0
                self.samples.append({"t": now, "rss": rss, "cores": cores})
            prev_cpu, prev_t = cpu, now
            self._done.wait(self.interval)

    def stop(self):
        self._done.set()
        self.join(timeout=2)

    def window(self, start, end):
        return [s for s in self.samples if start <= s["t"] <= end]


def summarize(samples, label):
    if not samples:
        return {"phase": label, "n": 0}
    rss = [s["rss"] for s in samples]
    cores = [s["cores"] for s in samples]
    return {
        "phase": label,
        "n": len(samples),
        "rss_mb_mean": sum(rss) / len(rss) / 1e6,
        "rss_mb_peak": max(rss) / 1e6,
        "cores_mean": sum(cores) / len(cores),
        "cores_peak": max(cores),
    }


def drive_session(sock_path, hold, marks):
    """跑一次完整的 start → stop → result。"""
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(30)
        s.connect(sock_path)
        rx = s.makefile("r", encoding="utf-8")

        marks["record_start"] = time.monotonic()
        s.sendall(
            json.dumps(
                {
                    "type": "start",
                    "session": 9001,
                    "program": "measure-resources",
                    "isPassword": False,
                }
            ).encode()
            + b"\n"
        )
        time.sleep(hold)

        marks["infer_start"] = time.monotonic()
        s.sendall(json.dumps({"type": "stop", "session": 9001}).encode() + b"\n")

        for line in rx:
            msg = json.loads(line)
            if msg.get("type") in ("result", "error"):
                marks["infer_end"] = time.monotonic()
                return msg
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--hold", type=float, default=3.0, help="錄音秒數")
    ap.add_argument("--idle", type=float, default=3.0, help="錄音前的閒置取樣秒數")
    ap.add_argument("--hz", type=float, default=20.0, help="取樣頻率")
    ap.add_argument("--json", metavar="PATH", help="另存機器可讀結果")
    ap.add_argument("--speak", action="store_true",
                    help="提示使用者在錄音期間說話 —— 沒有語音的話 VAD 會擋下, "
                         "量不到推論階段的資源")
    args = ap.parse_args()

    pids = find_daemon()
    if not pids:
        sys.exit("找不到執行中的 voicetyped。先啟動:\n"
                 "  systemctl --user start voicetyped")
    if len(pids) > 1:
        sys.exit(f"有多個 voicetyped 在跑 ({pids}), 量測結果會混淆。")
    pid = pids[0]

    sock = socket_path()
    if not os.path.exists(sock):
        sys.exit(f"找不到 socket: {sock}")

    print(f"daemon pid={pid}  socket={sock}")
    print(f"取樣 {args.hz}Hz · 閒置 {args.idle}s · 錄音 {args.hold}s")
    print("\n⚠ 這會開啟麥克風。音訊只留在記憶體, 不寫檔。")
    if args.speak:
        print(f"\n   >>> 倒數結束後請說一句話 ({args.hold:.0f} 秒) <<<")
    else:
        print("   (沒說話的話 VAD 會擋下, 量不到推論階段 —— 加 --speak)")
    print()

    sampler = Sampler(pid, 1.0 / args.hz)
    sampler.start()

    idle_start = time.monotonic()
    time.sleep(args.idle)
    idle_end = time.monotonic()

    marks = {}
    result = drive_session(sock, args.hold, marks)

    # 推論結束後再取樣一段, 看記憶體有沒有回落 (推論的計算緩衝是否釋放)。
    time.sleep(1.0)
    sampler.stop()

    phases = [
        summarize(sampler.window(idle_start, idle_end), "閒置"),
        summarize(
            sampler.window(marks["record_start"], marks["infer_start"]), "錄音中"
        ),
        summarize(sampler.window(marks["infer_start"], marks["infer_end"]), "推論中"),
        summarize(sampler.window(marks["infer_end"], time.monotonic()), "推論後"),
    ]

    print(f"{'階段':<8} {'RSS 平均':>10} {'RSS 尖峰':>10} {'CPU 平均':>10} {'CPU 尖峰':>10}")
    print("-" * 54)
    for p in phases:
        if not p["n"]:
            continue
        print(
            f"{p['phase']:<8} {p['rss_mb_mean']:>8.1f}MB {p['rss_mb_peak']:>8.1f}MB "
            f"{p['cores_mean']:>8.2f}核 {p['cores_peak']:>8.2f}核"
        )

    infer_ms = (marks["infer_end"] - marks["infer_start"]) * 1000
    print(f"\n放開熱鍵 → 結果: {infer_ms:.0f} ms")
    if result and result.get("type") == "result":
        print(f"辨識結果: {result.get('text')!r}")
    elif result:
        # 協定的錯誤欄位是 code/text (見 protocol::ServerMessage::Error)。
        print(f"daemon 回錯誤: {result.get('code')} — {result.get('text')}")
        if result.get("code") == "empty_result":
            print("  (沒說話的話這是正常的: VAD 判定無語音, 引擎完全沒被叫起來)")

    if args.json:
        with open(args.json, "w", encoding="utf-8") as f:
            json.dump(
                {"pid": pid, "hold_s": args.hold, "phases": phases,
                 "latency_ms": infer_ms, "result": result},
                f, ensure_ascii=False, indent=2,
            )
        print(f"\n已寫入 {args.json}")


if __name__ == "__main__":
    main()
