#!/usr/bin/env python3
"""Local, resumable recording page for a 100-sentence corpus. Microphone starts on click only."""
import argparse
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import secrets
import signal
import struct
import subprocess
import threading
import time
import wave
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = Path(__file__).resolve().parent
DEFAULT_CORPUS = HERE / "fixtures" / "dictation-demo.json"
DEFAULT_OUTPUT = HERE / "recordings" / "dictation-demo"


def load_corpus(path):
    data = json.loads(Path(path).read_text())
    cases = data["cases"]
    ids = [c["id"] for c in cases]
    if len(cases) != 100 or ids != [f"p{i:03}" for i in range(1, 101)]:
        raise ValueError("測驗必須正好包含 p001 到 p100")
    if sum(c["split"] == "dev" for c in cases) != 60 or sum(c["split"] == "test" for c in cases) != 40:
        raise ValueError("測驗必須為 60 dev / 40 test")
    if any(not c["reference"].strip() for c in cases):
        raise ValueError("缺少參考句")
    return data


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def audio_quality(path):
    with wave.open(str(path), "rb") as wav:
        if (wav.getnchannels(), wav.getsampwidth(), wav.getframerate()) != (1, 2, 16000):
            raise ValueError("錄音格式必須是 16kHz、單聲道、16-bit PCM")
        frames = wav.readframes(wav.getnframes())
    samples = struct.unpack(f"<{len(frames) // 2}h", frames)
    duration = len(samples) / 16000
    rms = math.sqrt(sum(s * s for s in samples) / max(1, len(samples))) / 32768
    peak = max((abs(s) for s in samples), default=0) / 32768
    clipped = sum(abs(s) >= 32760 for s in samples) / max(1, len(samples))
    if duration < 0.6:
        raise ValueError("錄音太短，請重錄")
    if rms < 0.0001:
        raise ValueError("錄音幾乎沒有聲音，請檢查麥克風後重錄")
    return {"seconds": round(duration, 3), "rms": round(rms, 6),
            "peak": round(peak, 6), "clipped_fraction": round(clipped, 6)}


class Recorder:
    def __init__(self, corpus, output, device=None):
        self.corpus_path = Path(corpus).resolve()
        self.corpus = load_corpus(self.corpus_path)
        self.cases = {c["id"]: c for c in self.corpus["cases"]}
        self.output = Path(output).resolve()
        self.output.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.corpus_hash = digest(self.corpus_path)
        frozen = self.output / "corpus.json"
        if frozen.exists() and digest(frozen) != self.corpus_hash:
            raise ValueError("題目已變更；請用不同的 --output，避免把兩版錄音混在一起")
        if not frozen.exists():
            frozen.write_bytes(self.corpus_path.read_bytes())
            frozen.chmod(0o600)
        self.device = device
        self.lock = threading.Lock()
        self.proc = None
        self.current = None
        self.take = self.output / ".take.wav"
        self.log = None
        self.quality = None
        self.generation = 0
        self.last_error = None

    def progress(self):
        done = []
        for cid in self.cases:
            wav, meta = self.output / f"{cid}.wav", self.output / f"{cid}.json"
            if not (wav.exists() and meta.exists()):
                continue
            try:
                entry = json.loads(meta.read_text())
                if (entry.get("confirmed_reading") is True and entry.get("corpus_sha256") == self.corpus_hash
                        and entry.get("wav_sha256") == digest(wav)):
                    done.append(cid)
            except (OSError, ValueError):
                pass
        return done

    def state(self):
        if self.proc is not None and self.proc.poll() is not None:
            try:
                self.stop()
            except (ValueError, OSError, wave.Error) as exc:
                self.last_error = str(exc)
        return {"cases": self.corpus["cases"], "done": self.progress(),
                "recording": self.proc is not None, "current": self.current,
                "quality": self.quality, "error": self.last_error}

    def start(self, cid):
        if cid not in self.cases:
            raise ValueError("題號無效")
        if self.proc is not None:
            raise ValueError("請先結束目前錄音")
        self.take.unlink(missing_ok=True)
        self.current, self.quality, self.last_error = cid, None, None
        self.log = open(self.output / ".arecord.log", "wb")
        cmd = ["arecord", "-q", "-t", "wav", "-f", "S16_LE", "-r", "16000", "-c", "1", "-d", "45"]
        if self.device:
            cmd += ["-D", self.device]
        cmd.append(str(self.take))
        try:
            self.proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=self.log)
        except OSError:
            self.log.close()
            self.log = None
            raise
        self.generation += 1
        generation = self.generation
        watchdog = threading.Timer(46, self.auto_stop, args=(generation,))
        watchdog.daemon = True
        watchdog.start()

    def auto_stop(self, generation):
        with self.lock:
            if self.proc is not None and self.generation == generation:
                try:
                    self.stop()
                except (ValueError, OSError, wave.Error) as exc:
                    self.last_error = str(exc)

    def stop(self):
        if self.proc is None:
            if self.quality is not None:
                return self.quality
            raise ValueError("目前沒有錄音")
        proc, self.proc = self.proc, None
        if proc.poll() is None:
            proc.send_signal(signal.SIGINT)
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        self.log.close()
        self.log = None
        try:
            self.quality = audio_quality(self.take)
        except (OSError, ValueError, wave.Error) as exc:
            details = (self.output / ".arecord.log").read_text(errors="replace")[-500:]
            raise ValueError(f"{exc}。{details}") from exc
        return self.quality

    def accept(self, cid, confirmed):
        if self.proc is not None or self.current != cid or self.quality is None or confirmed is not True:
            raise ValueError("請先結束錄音，確認照稿讀完且沒有漏字，再存檔")
        # A retake only replaces the prior accepted recording at this explicit action.
        wav = self.output / f"{cid}.wav"
        metadata = {"id": cid, "confirmed_reading": True, "reference": self.cases[cid]["reference"],
                    "corpus_sha256": self.corpus_hash, "wav_sha256": digest(self.take),
                    "quality": self.quality, "recorded_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
                    "capture": {"format": "PCM_S16LE", "sample_rate": 16000, "channels": 1,
                                "device": self.device or "system default"}}
        staged = self.output / f".{cid}.json.tmp"
        staged.write_text(json.dumps(metadata, ensure_ascii=False, indent=2) + "\n")
        staged.chmod(0o600)
        self.take.chmod(0o600)
        os.replace(self.take, wav)
        os.replace(staged, self.output / f"{cid}.json")
        self.current, self.quality = None, None

    def close(self):
        with self.lock:
            if self.proc is not None:
                try:
                    self.stop()
                except (ValueError, OSError, wave.Error):
                    pass


def handler_for(recorder, token):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, fmt, *args):
            pass

        def respond(self, status, data, mime="application/json; charset=utf-8"):
            if isinstance(data, (dict, list)):
                data = json.dumps(data, ensure_ascii=False).encode()
            if isinstance(data, str):
                data = data.encode()
            self.send_response(status)
            self.send_header("Content-Type", mime)
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("X-Frame-Options", "DENY")
            self.end_headers()
            self.wfile.write(data)

        def valid_host(self):
            return self.headers.get("Host") == f"127.0.0.1:{self.server.server_port}"

        def do_GET(self):
            if not self.valid_host():
                self.respond(403, {"error": "請使用顯示的 127.0.0.1 網址"})
                return
            path = self.path.split("?", 1)[0]
            if path == "/":
                html = (HERE / "record_personal.html").read_text().replace("__TOKEN__", token)
                self.respond(200, html, "text/html; charset=utf-8")
            elif path == "/api/state" and secrets.compare_digest(self.headers.get("X-Recorder-Token", ""), token):
                with recorder.lock:
                    self.respond(200, recorder.state())
            elif path.startswith("/audio/"):
                cid = path.removeprefix("/audio/").removesuffix(".wav")
                with recorder.lock:
                    file = recorder.take if cid == "take" and recorder.quality else recorder.output / f"{cid}.wav"
                    if (cid in recorder.cases or cid == "take") and file.is_file() and not (cid == "take" and recorder.proc):
                        self.respond(200, file.read_bytes(), "audio/wav")
                    else:
                        self.respond(404, {"error": "尚無錄音"})
            else:
                self.respond(404, {"error": "not found"})

        def do_POST(self):
            expected_origin = f"http://127.0.0.1:{self.server.server_port}"
            if (not self.valid_host() or self.headers.get("Origin", expected_origin) != expected_origin
                    or not secrets.compare_digest(self.headers.get("X-Recorder-Token", ""), token)):
                self.respond(403, {"error": "無效的錄音頁面要求"})
                return
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if not 0 <= length <= 2048:
                    raise ValueError("request too large")
                data = json.loads(self.rfile.read(length))
                with recorder.lock:
                    if self.path == "/api/start":
                        recorder.start(data.get("id"))
                    elif self.path == "/api/stop":
                        recorder.stop()
                    elif self.path == "/api/accept":
                        recorder.accept(data.get("id"), data.get("confirmed"))
                    else:
                        self.respond(404, {"error": "not found"})
                        return
                    self.respond(200, recorder.state())
            except (ValueError, OSError, wave.Error, AttributeError) as exc:
                self.respond(400, {"error": str(exc)})
    return Handler


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", type=Path, default=DEFAULT_CORPUS)
    ap.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    ap.add_argument("--device", help="ALSA input device (default: system default)")
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("--list", action="store_true", help="Show verified recording progress")
    args = ap.parse_args()
    os.umask(0o077)
    recorder = Recorder(args.corpus, args.output, args.device)
    if args.list:
        print(f"已確認錄音：{len(recorder.progress())}/100\n資料夾：{recorder.output}")
        return
    process_lock = open(recorder.output / ".recorder.lock", "w")
    try:
        fcntl.flock(process_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        raise SystemExit("這份測驗已經有錄音頁面在執行，請使用原本的頁面")
    server = ThreadingHTTPServer(("127.0.0.1", args.port), handler_for(recorder, secrets.token_hex(24)))
    print(f"錄音頁面：http://127.0.0.1:{server.server_port}\n按頁面上的開始才會開啟麥克風。Ctrl+C 關閉。", flush=True)
    def terminate(signum, frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, terminate)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        recorder.close()
        server.server_close()
        process_lock.close()


if __name__ == "__main__":
    main()
