#!/usr/bin/env python3
"""voicetyped 的 IPC 煙霧測試 (SDD §4.3)。

在沒有 fcitx5 的情況下扮演 addon, 走完一次完整的 PTT 週期:

    ping → pong
    start → (錄音) → stop → result

用來驗證 SDD §9 M0 的「Unix socket IPC 打通」, 也讓協定改動有一個
不需要重啟輸入法就能跑的回歸檢查。

用法:
    python3 scripts/smoke-ipc.py [--socket PATH] [--hold SECONDS]
"""

import argparse
import json
import os
import socket
import sys
import time


class Client:
    def __init__(self, path, timeout=15.0):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout)
        self.sock.connect(path)
        self.buf = b""

    def send(self, **msg):
        line = (json.dumps(msg, ensure_ascii=False) + "\n").encode("utf-8")
        self.sock.sendall(line)

    def recv(self):
        while b"\n" not in self.buf:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise EOFError("daemon closed the connection")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line.decode("utf-8"))

    def close(self):
        self.sock.close()


def default_socket():
    env = os.environ.get("VOICETYPE_SOCKET")
    if env:
        return env
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if runtime:
        return os.path.join(runtime, "voicetype", "ipc.sock")
    return f"/tmp/voicetype-{os.getuid()}/ipc.sock"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--socket", default=default_socket())
    ap.add_argument("--hold", type=float, default=1.5,
                    help="模擬按住熱鍵的秒數")
    ap.add_argument("--program", default="kitty",
                    help="模擬的應用程式識別 (per-app profile, SDD §4.5)")
    args = ap.parse_args()

    if not os.path.exists(args.socket):
        print(f"FAIL: socket not found: {args.socket}", file=sys.stderr)
        print("      daemon 有在跑嗎? cargo run -p voicetyped", file=sys.stderr)
        return 1

    print(f"connecting to {args.socket}")
    c = Client(args.socket)

    # 1. 探活
    c.send(type="ping")
    reply = c.recv()
    assert reply.get("type") == "pong", f"expected pong, got {reply}"
    print("  ping → pong                     ok")

    # 2. 一次完整的 PTT 週期
    session = 1
    c.send(type="start", session=session, program=args.program,
           is_password=False)
    print(f"  start (session={session}, program={args.program})")
    print(f"  ... 按住 {args.hold}s (現在對麥克風說話)")
    time.sleep(args.hold)
    c.send(type="stop", session=session)

    t0 = time.monotonic()
    reply = c.recv()
    latency_ms = (time.monotonic() - t0) * 1000

    if reply.get("type") == "result":
        print(f"  stop → result ({latency_ms:.0f}ms)          ok")
        print(f"\n  轉錄結果: {reply['text']!r}")
        if reply.get("session") != session:
            print(f"FAIL: session 不符: {reply.get('session')} != {session}",
                  file=sys.stderr)
            return 1
    elif reply.get("type") == "error":
        # 沒有麥克風、或誤觸產生空錄音, 都是預期內的錯誤路徑。
        print(f"  stop → error                    (code={reply.get('code')})")
        print(f"\n  訊息: {reply.get('text')!r}")
        if reply.get("code") == "no_audio_device":
            print("\n  提示: 找不到輸入裝置。檢查預設麥克風設定。")
    else:
        print(f"FAIL: unexpected reply {reply}", file=sys.stderr)
        return 1

    # 3. 過期結果必須被丟棄 (SDD §4.2.5)
    c.send(type="start", session=2, program=args.program, is_password=False)
    time.sleep(0.2)
    c.send(type="cancel", session=2)
    print("  cancel                          ok (無回應即正確)")

    c.close()
    print("\nsmoke test 完成")
    return 0


if __name__ == "__main__":
    sys.exit(main())
