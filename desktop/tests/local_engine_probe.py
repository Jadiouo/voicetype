"""Probe a real Nano CPU daemon in an isolated profile, without opening audio.

Arguments: candidate-binary model-directory silero-model. Uses existing read-only
model assets; never downloads, logs in, sends Start or uses personal settings.
This verifies runtime/control integration, not speech or latency acceptance.
"""
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time


def main():
    if len(sys.argv) != 4 or sys.platform != "linux":
        raise SystemExit("Linux usage: local_engine_probe.py BINARY MODEL_DIR VAD_MODEL")
    binary, model, vad = [Path(arg).resolve(strict=True) for arg in sys.argv[1:]]
    with tempfile.TemporaryDirectory(prefix="voicetype-probe-") as temporary:
        root = Path(temporary)
        endpoint = root / "run" / "voicetype" / "ipc.sock"
        for directory in ("home", "config", "data", "run"):
            (root / directory).mkdir(mode=0o700)
        env = {
            "PATH": os.defpath,
            "HOME": str(root / "home"), "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"), "XDG_RUNTIME_DIR": str(root / "run"),
            "LANG": "C.UTF-8", "CUDA_VISIBLE_DEVICES": "",
            "VOICETYPE_ASR_PROFILE": "nano", "VOICETYPE_NANO_MODEL_DIR": str(model),
            "VOICETYPE_NANO_VAD_MODEL": str(vad), "VOICETYPE_SOCKET": str(endpoint),
        }
        with (root / "engine.log").open("w+") as log:
            process = subprocess.Popen([str(binary)], env=env, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 30
                while not endpoint.exists():
                    if process.poll() is not None:
                        raise RuntimeError("candidate exited before binding the isolated socket")
                    if time.monotonic() >= deadline:
                        raise TimeoutError("candidate did not bind within 30 seconds")
                    time.sleep(0.05)
                with socket.socket(socket.AF_UNIX) as peer:
                    peer.settimeout(6)
                    peer.connect(str(endpoint))
                    with peer.makefile("rb") as stream:
                        def command(value):
                            peer.sendall(json.dumps(value).encode() + b"\n")
                            line = stream.readline(4097)
                            assert len(line) <= 4096 and line.endswith(b"\n")
                            return json.loads(line)
                        assert command({"type": "desktop_status", "request": 1}) == {
                            "type": "info", "value": {"desktop_protocol": 1, "request": 1,
                                "capabilities": ["session_events", "suspend"], "session_busy": False}}
                        assert command({"type": "desktop_suspend", "request": 2}) == {
                            "type": "info", "value": {"desktop_protocol": 1, "request": 2,
                                                      "microphone": "closed"}}
                        assert command({"type": "ping"}) == {"type": "pong"}
                maps = Path(f"/proc/{process.pid}/maps").read_text().lower()
                assert "libsherpa-onnx-c-api.so" in maps
                assert "libonnxruntime.so" in maps
                assert not any(name in maps for name in ("libcuda", "libcudnn", "libnvinfer"))
                log.flush()
                log.seek(0)
                diagnostic = log.read()
                assert "funasr-nano:int8:cpu" in diagnostic
                assert "provider=cpu" in diagnostic
                assert "opening input stream" not in diagnostic
                process.send_signal(signal.SIGINT)
                assert process.wait(timeout=10) == 0
                assert not endpoint.exists(), "candidate left its socket behind"
                print(json.dumps({"nano_cpu_loaded": True, "desktop_protocol": 1,
                                  "suspend_acknowledged": True, "microphone_opened": False,
                                  "gpu_libraries_loaded": False, "clean_exit": True}))
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)


if __name__ == "__main__":
    main()
