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
import wave


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
        env["XDG_DATA_HOME"] = str(Path(profile) / "data")
        env["XDG_CONFIG_HOME"] = str(Path(profile) / "legacy-config")
        env["VOICETYPE_VOCAB"] = str(Path(profile) / "legacy-vocab.toml")
        Path(env["VOICETYPE_VOCAB"]).write_text("# legacy fixture\nfuture=23\nentry=[]\n", encoding="utf-8")
        registration = Path(profile) / "data/fcitx5/addon/voicetype.conf"
        registration.parent.mkdir(parents=True)
        previous_registration = b"[Addon]\nLibrary=/fixture/previous/libvoicetype\n"
        registration.write_bytes(previous_registration)
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
                assert js("return document.querySelector('#spelling-enabled').checked")
                assert not js("return document.querySelector('#spelling-enabled').disabled")
                click("#spelling-enabled")
                spelling_config = Path(profile) / "desktop.json"
                eventually(lambda: spelling_config.exists() and json.loads(spelling_config.read_text())["spelling_enabled"] is False)
                assert not js("return document.querySelector('#spelling-enabled').checked")
                assert js("return document.querySelector('#recovery-note').textContent.includes('沒有待處理')")
                assert js("return document.querySelector('#recovery-content').hidden")
                eventually(lambda: js("return document.querySelector('#prepare-models').disabled === false"))
                assert js("return document.querySelector('#model-status').textContent.includes('尚未檢查模型')")
                assert js("return document.querySelector('#cancel-models').hidden")
                assert js("return document.querySelector('#model-progress').hidden")
                assert not (Path(profile) / "model-assets").exists(), "Opening settings started model setup"
                assert not (Path(profile) / "runtime-assets").exists(), "Opening settings installed a runtime"
                assert not (Path(profile) / "local-profile").exists(), "Opening settings launched an engine"
                assert js("return document.querySelector('#enable-local-input').disabled"), "Input enabled before runtime"
                if sys.platform == "linux":
                    click("#install-input-module")
                    eventually(lambda: js("return document.querySelector('#input-module-status').textContent.includes('模組已安裝')"))
                    installed_registration = registration.read_text()
                    assert "input-assets/versions/" in installed_registration
                    assert (Path(profile) / "fcitx-rollback.json").is_file()
                    assert not (Path(profile) / "local-profile").exists()
                    click("#restore-input-module")
                    eventually(lambda: js("return document.querySelector('#input-module-status').textContent.includes('已還原')"))
                    assert registration.read_bytes() == previous_registration
                    assert not (Path(profile) / "fcitx-rollback.json").exists()
                    eventually(lambda: js("return !document.querySelector('#load-local-runtime').disabled"))
                    click("#load-local-runtime")
                    eventually(lambda: js("return !document.querySelector('#error').hidden"))
                    assert js("return document.querySelector('#error').textContent.includes('請先下載／檢查模型')")
                    assert not (Path(profile) / "runtime-assets").exists()
                    assert not (Path(profile) / "local-profile").exists()
                    click("#reload")
                    eventually(ready)
                else:
                    assert js("return document.querySelector('#load-local-runtime').disabled")
                    assert js("return document.querySelector('#runtime-note').textContent.includes('仍在準備中')")
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
                # Exercise the actual vocabulary commands through the installed
                # webview. Synthetic words only; no live dictionary is accessed.
                eventually(lambda: js("return !document.querySelector('#vocab-fields').disabled"))
                assert not (Path(profile) / "vocab.toml").exists(), "Opening the app wrote a vocabulary"
                click("#vocab-import")
                eventually(lambda: js("return document.querySelector('#vocab-status').textContent.includes('已儲存')"))
                assert (Path(profile) / "vocab.toml").read_bytes() == Path(env["VOICETYPE_VOCAB"]).read_bytes()
                js("document.querySelector('#vocabulary details').open = true")
                js("document.querySelector('#vocab-names').value = '台積電\\n游錫堃'")
                assert js("return !document.querySelector('#vocab-save-names').disabled"), "OpenCC not loaded"
                click("#vocab-save-names")
                eventually(lambda: js("return document.querySelector('#vocab-status').textContent.includes('已儲存')"))
                js("document.querySelector('#vocab-preview-input').value = '臺積電與游錫堃'")
                click("#vocab-preview")
                eventually(lambda: js("return document.querySelector('#vocab-preview-output').textContent === '台積電與游錫堃'"))
                imported_names = (Path(profile) / "vocab.toml").read_bytes()
                click("#vocab-example")
                click("#vocab-save")
                eventually(lambda: js("return document.querySelector('#vocab-list').textContent.includes('geeho、git hub → GitHub')"))
                vocab_saved = (Path(profile) / "vocab.toml").read_bytes()
                assert b"# legacy fixture" in vocab_saved and b"future=23" in vocab_saved
                js("document.querySelector('#vocab-preview-input').value = '先 push 到 GEEHO，coming soon；`geeho` 保留。'")
                click("#vocab-preview")
                eventually(lambda: js("return document.querySelector('#vocab-preview-output').textContent === '先 push 到 GitHub，coming soon；`geeho` 保留。'"))
                # Editing another copy invalidates the displayed revision.
                (Path(profile) / "vocab.toml").write_bytes(vocab_saved + b"# external editor\n")
                click("#vocab-list button")
                js("document.querySelector('#vocab-right').value = 'Other'")
                click("#vocab-save")
                eventually(lambda: js("return !document.querySelector('#vocab-error').hidden"))
                assert js("return document.querySelector('#vocab-error').textContent.includes('重新載入')")
                assert (Path(profile) / "vocab.toml").read_bytes() == vocab_saved + b"# external editor\n"
                assert js("return document.querySelector('#vocab-right').value === 'Other'")
                click("#vocab-reload")
                eventually(lambda: js("return document.querySelector('#vocab-status').textContent === '詞庫已載入。'"))
                click("#vocab-restore")
                eventually(lambda: js("return document.querySelector('#vocab-status').textContent.includes('已儲存')"))
                assert (Path(profile) / "vocab.toml").read_bytes() == imported_names
                click("#vocab-restore")
                eventually(lambda: js("return document.querySelector('#vocab-list').textContent.includes('geeho、git hub → GitHub')"))
                js("document.querySelector('#vocabulary').scrollIntoView()")
                (output / "vocabulary.png").write_bytes(base64.b64decode(request("GET", f"/session/{session}/screenshot")))
                # Synthetic review data exercises the actual native commands,
                # WAV decoder and shared vocabulary. No microphone is opened.
                eventually(lambda: js("return !document.querySelector('#review-enabled').disabled"))
                assert not js("return document.querySelector('#review-enabled').checked")
                review_config = Path(profile) / "review.json"
                review_root = Path(profile) / "review"
                assert not review_config.exists() and not review_root.exists()
                click("#review-enabled")
                eventually(lambda: review_config.exists() and json.loads(review_config.read_text())["enabled"])
                assert json.loads(review_config.read_text())["daily_limit"] == 5
                timestamp = int(time.time())
                review_id = f"r-{timestamp}-1-1"
                record = {"version": 1, "id": review_id, "created_at": timestamp,
                          "duration_ms": 9000, "sample_rate": 16000, "status": "pending",
                          "asr_text": "請用 RUSTT 編譯，保留 GitHub。",
                          "output_text": "請用 RUSTT 編譯，保留 GitHub。", "corrected_text": None,
                          "future": "preserved"}
                sample_dir = review_root / review_id
                sample_dir.mkdir(parents=True)
                record_path = sample_dir / "record.json"
                record_path.write_text(json.dumps(record, ensure_ascii=False), encoding="utf-8")
                with wave.open(str(sample_dir / "audio.wav"), "wb") as wav:
                    wav.setnchannels(1); wav.setsampwidth(2); wav.setframerate(16000)
                    wav.writeframes(b"\0\0" * 144000)
                expired = review_root / "r-1-1-1"
                expired.mkdir()
                (expired / "record.json").write_text(json.dumps({**record, "id": "r-1-1-1", "created_at": timestamp - 7 * 86400}), encoding="utf-8")
                click("#review-reload")
                eventually(lambda: js("return document.querySelector('#review-count').textContent === '(1)'"))
                assert not expired.exists()
                click("#review-list button")
                click("#review-audio-load")
                eventually(lambda: js("return document.querySelector('#review-audio').readyState >= 1"))
                assert js("return document.querySelector('#review-audio').duration") == 9
                assert js("return document.querySelector('#review-audio').paused"), "Audio started without Play"
                click("#review-save")
                eventually(lambda: json.loads(record_path.read_text(encoding="utf-8"))["status"] == "correct")
                eventually(lambda: js("return !document.querySelector('#review-save').disabled"))
                js("document.querySelector('#review-corrected').value = '請用 Rust 編譯，保留 GitHub。'; document.querySelector('#review-corrected').dispatchEvent(new Event('input'))")
                click("#review-save")
                eventually(lambda: js("return !document.querySelector('#review-promote').hidden"))
                assert "RUSTT" not in (Path(profile) / "vocab.toml").read_text(encoding="utf-8")
                click("#review-promote")
                eventually(lambda: js("return document.querySelector('#review-status').textContent.includes('規則已加入詞庫')"))
                assert "RUSTT" in (Path(profile) / "vocab.toml").read_text(encoding="utf-8")
                saved_review = json.loads(record_path.read_text(encoding="utf-8"))
                assert saved_review["future"] == "preserved" and saved_review["promotion"]["state"] == "applied"
                saved_review["future"] = "external edit"
                record_path.write_text(json.dumps(saved_review), encoding="utf-8")
                js("document.querySelector('#review-corrected').value = 'unsaved change'; document.querySelector('#review-corrected').dispatchEvent(new Event('input'))")
                click("#review-save")
                eventually(lambda: js("return !document.querySelector('#review-error').hidden"))
                assert js("return document.querySelector('#review-corrected').value") == "unsaved change"
                assert json.loads(record_path.read_text(encoding="utf-8"))["corrected_text"] == "請用 Rust 編譯，保留 GitHub。"
                click("#review-reload")
                eventually(lambda: js("return document.querySelector('#review-status').textContent === '校對資料已載入。'"))
                js("document.querySelector('#review').scrollIntoView()")
                (output / "review.png").write_bytes(base64.b64decode(request("GET", f"/session/{session}/screenshot")))
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
                assert not js("return document.querySelector('#spelling-enabled').checked"), "CSC choice did not survive restart"
                click("#spelling-enabled")
                eventually(lambda: json.loads(spelling_config.read_text())["spelling_enabled"] is True)
                assert js("return document.querySelector('#spelling-enabled').checked")

                eventually(lambda: js("return document.querySelector('#vocab-list').textContent.includes('geeho、git hub → GitHub')"))

                eventually(lambda: js("return document.querySelector('#review-enabled').checked"))
                click("#review-list button")
                assert js("return document.querySelector('#review-corrected').value") == "請用 Rust 編譯，保留 GitHub。"
                click("#review-delete")
                eventually(lambda: not sample_dir.exists())
                click("#review-enabled")
                eventually(lambda: json.loads(review_config.read_text())["enabled"] is False)

                # Corruption is a public persisted-config input. The UI must show
                # an error and disable writes instead of silently resetting it.
                config = Path(profile) / "desktop.json"
                original = config.read_bytes()
                config.write_text("{invalid", encoding="utf-8")
                click("#reload")
                eventually(lambda: js("return !document.querySelector('#error').hidden"))
                assert js("return document.querySelector('#providers').disabled")
                assert js("return document.querySelector('#spelling-enabled').disabled")
                assert config.read_text(encoding="utf-8") == "{invalid"
                config.write_bytes(original)
                click("#reload")
                eventually(ready)
                assert js("return document.querySelector('#provider-google').checked")
                assert js("return document.querySelector('#spelling-enabled').checked")
                (output / "result.json").write_text(json.dumps({
                    "passed": ["installed-window", "default-local", "choose-google",
                               "restart-persists-choice", "corrupt-settings-visible", "repair-and-reload",
                               "provider-status-without-recording", "recovery-empty-state",
                               "model-setup-explicit-only", "runtime-setup-requires-models", "input-requires-runtime", "vocabulary-explicit-import",
                               "vocabulary-save-preview", "vocabulary-conflict-preserves-edit",
                               "vocabulary-restore-restart", "vocabulary-protected-names",
                               "review-explicit-opt-in", "review-expiry", "review-wav-playback-ready",
                               "review-confirm-correct-promote", "review-conflict-preserves-edit",
                               "review-restart-delete-disable"],
                    "spelling_toggle_restart_and_corrupt_settings": True,
                    "speech_adapters_tested": False,
                    "fcitx_install_restore_tested": sys.platform == "linux",
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
