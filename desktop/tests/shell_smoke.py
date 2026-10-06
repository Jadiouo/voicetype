"""Exercise the packaged UI through a real OS WebDriver, with an isolated profile.

Requires tauri-driver and the platform's native WebDriver. No mock Tauri bridge,
microphone, user login, GPU inference or dictation/input injection is involved.
"""
import base64
import http.client
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request


def main():
    binary = Path(sys.argv[1]).resolve(strict=True)
    output = Path("target/shell-evidence")
    output.mkdir(parents=True, exist_ok=True)
    executable = shutil.which("tauri-driver")
    if not executable:
        raise SystemExit("tauri-driver is required")

    def request(method, route, body=None, timeout=15):
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request("http://127.0.0.1:4444" + route, data=data,
                                     method=method, headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=timeout) as response:
                return json.load(response)["value"]
        except urllib.error.HTTPError as error:
            raise RuntimeError(error.read().decode(errors="replace")) from error

    def eventually(check, timeout=30):
        deadline = time.monotonic() + timeout
        last_error = None
        while time.monotonic() < deadline:
            try:
                value = check()
                if value:
                    return value
            except (urllib.error.URLError, http.client.RemoteDisconnected, RuntimeError, TimeoutError) as error:
                last_error = error
            time.sleep(0.2)
        raise AssertionError(f"UI condition timed out: {last_error}")

    with tempfile.TemporaryDirectory(prefix="voicetype-shell-") as profile:
        env = os.environ.copy()
        env["VOICETYPE_PREVIEW_CONFIG_DIR"] = profile
        # Probe a disposable endpoint, never a developer's running engine.
        engine_socket = Path(profile) / "engine.sock"
        env["VOICETYPE_SOCKET"] = str(engine_socket)
        # Rendering under CI/Xvfb uses software; this is not an ASR benchmark.
        env["LIBGL_ALWAYS_SOFTWARE"] = "1"
        env["WEBKIT_DISABLE_DMABUF_RENDERER"] = "1"
        with (output / "webdriver.log").open("w", encoding="utf-8") as log:
            process = subprocess.Popen([executable], env=env, stdout=log, stderr=log)
            session = None
            try:
                # tauri-driver can bind before its native driver is listening.
                # Retry only this read-only readiness probe/condition checks;
                # session creation and clicks are never automatically replayed.
                eventually(lambda: request("GET", "/status").get("ready") is True)

                def open_app():
                    result = request("POST", "/session", {"capabilities": {
                        "alwaysMatch": {"browserName": "wry", "tauri:options": {"application": str(binary)}}
                    }}, timeout=90)
                    return result["sessionId"]

                def js(script):
                    return request("POST", f"/session/{session}/execute/sync", {"script": script, "args": []})

                def click(selector):
                    element = request("POST", f"/session/{session}/element", {"using": "css selector", "value": selector})
                    key = element["element-6066-11e4-a52e-4f735466cecf"]
                    request("POST", f"/session/{session}/element/{key}/click", {})

                def ready():
                    return js("return document.querySelector('#providers')?.disabled === false")

                session = open_app()
                eventually(ready)
                assert js("return document.querySelector('#provider-local').checked")
                assert js("return document.querySelectorAll('.availability').length") == 2
                click("#check-providers")
                eventually(lambda: ready() and js("return document.querySelector('#status').textContent.includes('已檢查服務')"))
                if sys.platform == "linux":
                    assert js("return document.querySelector('#local-availability').textContent.includes('找不到本機服務')")
                    # Only substitute the external provider IPC. The installed
                    # Tauri command and UI run normally through the OS WebDriver.
                    commands = []
                    with socket.socket(socket.AF_UNIX) as listener:
                        listener.bind(str(engine_socket))
                        listener.listen(1)
                        listener.settimeout(15)

                        def reply_to_probe():
                            with listener.accept()[0] as peer:
                                peer.settimeout(5)
                                with peer.makefile("rb") as stream:
                                    commands.append(json.loads(stream.readline(4096)))
                                peer.sendall(b'{"type":"pong"}\n')

                        server = threading.Thread(target=reply_to_probe, daemon=True)
                        server.start()
                        click("#check-providers")
                        eventually(lambda: js("return document.querySelector('#local-availability').textContent.includes('本機服務有回應')"))
                        server.join(timeout=5)
                        assert commands == [{"type": "ping"}]
                    engine_socket.unlink()
                else:
                    assert js("return document.querySelector('#local-availability').textContent.includes('尚未連接')")
                assert js("return document.querySelector('#google-availability').textContent.includes('尚未連接')")
                click("#provider-google")
                eventually(lambda: ready() and js("return document.querySelector('#status').textContent.includes('偏好已儲存')"))
                assert js("return document.querySelector('#provider-google').checked")
                screenshot = request("GET", f"/session/{session}/screenshot")
                (output / "google-selected.png").write_bytes(base64.b64decode(screenshot))
                request("DELETE", f"/session/{session}")
                session = None
                session = open_app()
                eventually(ready)
                assert js("return document.querySelector('#provider-google').checked"), "Choice did not survive restart"

                # Corruption is a public persisted-config input. The UI must show
                # an error and disable writes instead of silently resetting it.
                config = Path(profile) / "desktop.json"
                original = config.read_bytes()
                config.write_text("{invalid", encoding="utf-8")
                click("#reload")
                eventually(lambda: js("return !document.querySelector('#error').hidden"))
                assert js("return document.querySelector('#providers').disabled")
                assert config.read_text(encoding="utf-8") == "{invalid"
                config.write_bytes(original)
                click("#reload")
                eventually(ready)
                assert js("return document.querySelector('#provider-google').checked")
                (output / "result.json").write_text(json.dumps({
                    "passed": ["installed-window", "default-local", "choose-google",
                               "restart-persists-choice", "corrupt-settings-visible", "repair-and-reload",
                               "provider-status-without-recording"],
                    "speech_adapters_tested": False,
                }, indent=2) + "\n", encoding="utf-8")
                print("PASS: real UI selection, restart, corruption and reload")
            finally:
                if session:
                    try:
                        request("DELETE", f"/session/{session}")
                    except Exception:
                        pass
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)


if __name__ == "__main__":
    main()
