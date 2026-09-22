#!/usr/bin/env python3
"""Control the running VoiceType daemon without opening the microphone."""
import argparse
import json
import os
from pathlib import Path
import socket
import sys


def request(message, socket_path=None):
    path = socket_path or os.environ.get("VOICETYPE_SOCKET") or str(
        Path(os.environ.get("XDG_RUNTIME_DIR", f"/tmp/voicetype-{os.getuid()}"))
        / ("voicetype/ipc.sock" if os.environ.get("XDG_RUNTIME_DIR") else "ipc.sock"))
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(15)
        sock.connect(path)
        sock.sendall(json.dumps(message, ensure_ascii=False).encode() + b"\n")
        data = bytearray()
        while b"\n" not in data:
            chunk = sock.recv(4096)
            if not chunk:
                raise RuntimeError("daemon disconnected without a response")
            data.extend(chunk)
            if len(data) > 1024 * 1024:
                raise RuntimeError("oversized response")
    result = json.loads(data.split(b"\n", 1)[0])
    if result.get("type") == "error":
        raise RuntimeError(result.get("text", "daemon error"))
    return result.get("value", result)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--socket")
    sub = ap.add_subparsers(dest="command", required=True)
    p = sub.add_parser("learn", help="Remember an explicit wrong → correct term")
    p.add_argument("wrong"); p.add_argument("right"); p.add_argument("--program", default="")
    sub.add_parser("learned", help="List active and pending personal rules")
    p = sub.add_parser("forget", help="Remove a learned rule")
    p.add_argument("wrong"); p.add_argument("--context-id")
    p = sub.add_parser("context", help="Set temporary context for 30 minutes")
    g = p.add_mutually_exclusive_group(required=True)
    g.add_argument("--text"); g.add_argument("--file", type=Path)
    p.add_argument("--program", default="")
    p = sub.add_parser("process", help="Preview the real text pipeline without typing")
    p.add_argument("text"); p.add_argument("--context", default="")
    p.add_argument("--program", default=""); p.add_argument("--context-id", default="")
    p.add_argument("--selected", default="")
    p.add_argument("--mode", choices=["off", "faithful", "clean"])
    sub.add_parser("ping")
    args = ap.parse_args()
    if args.command == "learn":
        message = dict(type="learn", wrong=args.wrong, right=args.right, program=args.program)
    elif args.command == "learned": message = dict(type="list_learned")
    elif args.command == "forget": message = dict(type="forget_learned", wrong=args.wrong, context_id=args.context_id)
    elif args.command == "context":
        text = args.file.read_text() if args.file else args.text
        message = dict(type="set_context", text=text, program=args.program)
    elif args.command == "process":
        message = dict(type="process_text", text=args.text, context_text=args.context,
                       program=args.program, context_id=args.context_id,
                       selected_text=args.selected, mode=args.mode)
    else: message = dict(type="ping")
    print(json.dumps(request(message, args.socket), ensure_ascii=False, indent=2))


if __name__ == "__main__":
    try: main()
    except (OSError, RuntimeError, ValueError) as exc:
        sys.exit(f"VoiceType: {exc}")
