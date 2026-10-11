"""Two-real-NSIS-package upgrade, rollback and data-retention gate.

Run only on a disposable GitHub Windows runner. Both installer paths and SHA256
values are mandatory; the runner workspace contains all test profile/evidence
files. No microphone, daily profile, or system installation is used.

Example (after building two genuinely different versions):
  python tests/windows_upgrade_smoke.py --old-installer target/old/setup.exe \
    --old-sha256 HASH --old-version 0.2.0-alpha.1 \
    --new-installer target/new/setup.exe --new-sha256 HASH --new-version 0.2.1-alpha.1
"""
import argparse
from collections import deque
import hashlib
import http.client
from itertools import islice
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import time
import urllib.error
import urllib.request
import wave


REVIEW_ID = "r-20261011-1-1"
FILES = (
    "desktop.json", "vocab.toml", "local-profile/data/learning.json",
    "review.json", f"review/{REVIEW_ID}/record.json",
    f"review/{REVIEW_ID}/audio.wav",
)


def require_within(path, workspace):
    path = Path(path).resolve(strict=True)
    if not path.is_relative_to(workspace):
        raise RuntimeError(f"Input is outside GITHUB_WORKSPACE: {path}")
    return path


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def powershell_json(script, env, timeout=30):
    result = subprocess.run(
        ["pwsh", "-NoProfile", "-NonInteractive", "-Command", script],
        env=env, capture_output=True, text=True, timeout=timeout, check=True,
    )
    return json.loads(result.stdout)


def file_version(path, env, expected):
    child_env = {**env, "VT_UPGRADE_FILE": str(path)}
    value = powershell_json(
        "$v=(Get-Item -LiteralPath $env:VT_UPGRADE_FILE).VersionInfo; "
        "@{file_string=$v.FileVersion;product_string=$v.ProductVersion;"
        "file_fixed=@($v.FileMajorPart,$v.FileMinorPart,$v.FileBuildPart,$v.FilePrivatePart);"
        "product_fixed=@($v.ProductMajorPart,$v.ProductMinorPart,$v.ProductBuildPart,$v.ProductPrivatePart)}"
        "|ConvertTo-Json -Compress",
        child_env,
    )
    fixed = value["file_fixed"]
    product = value["product_fixed"]
    if (value["file_string"] != expected or value["product_string"] != expected
            or not all(isinstance(parts, list) and len(parts) == 4 and all(
                isinstance(part, int) and not isinstance(part, bool) and 0 <= part <= 65535
                for part in parts) for parts in (fixed, product))
            or fixed != product):
        raise RuntimeError(f"PE string/fixed version mismatch: {path.name}: {value}")
    return tuple(fixed), value


def registration(destination, env):
    child_env = {**env, "VT_UPGRADE_DEST": str(destination)}
    script = r'''
$items = @(Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall' |
  ForEach-Object { Get-ItemProperty -LiteralPath $_.PSPath } |
  Where-Object { $_.DisplayName -like 'VoiceType Preview*' -and
    ($_.InstallLocation -eq $env:VT_UPGRADE_DEST -or
     $_.UninstallString -like "*$env:VT_UPGRADE_DEST*") })
if ($items.Count -ne 1) { throw "Expected exactly one owned NSIS registration; found $($items.Count)" }
@{display=$items[0].DisplayVersion;uninstall=$items[0].UninstallString;
  location=$items[0].InstallLocation}|ConvertTo-Json -Compress
'''
    return powershell_json(script, child_env)


def product_registration_count(env):
    script = r'''
$items = @(Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall' |
  ForEach-Object { Get-ItemProperty -LiteralPath $_.PSPath } |
  Where-Object { $_.DisplayName -like 'VoiceType Preview*' })
@{count=$items.Count}|ConvertTo-Json -Compress
'''
    return powershell_json(script, env)["count"]


def known_folder_product_paths(env, scratch):
    locations = powershell_json(
        "@{roaming=[Environment]::GetFolderPath('ApplicationData');"
        "local=[Environment]::GetFolderPath('LocalApplicationData')}|ConvertTo-Json -Compress",
        env,
    )
    return {f"{kind}/{name}": (root / name).exists()
            for kind, value in locations.items() if value
            for root in [Path(value)]
            for name in ("VoiceType Preview", "voicetype", "io.github.jadiouo.voicetype.preview")
            if not (root / name).is_relative_to(scratch)}


def bounded_folder_metadata(root):
    """Record names/types only; never read product or WebView file contents."""
    try:
        root.lstat()
    except FileNotFoundError:
        return {"exists": False, "entries": [], "truncated": False}
    except OSError as error:
        return {"exists": True, "entries": [{"path": ".", "error": type(error).__name__}],
                "truncated": False}
    entries = []
    pending = deque([(root, 0)])
    truncated = False
    while pending and len(entries) < 80:
        path, depth = pending.popleft()
        entry = None
        try:
            info = path.lstat()
            reparse = bool(getattr(info, "st_file_attributes", 0) &
                           getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0))
            kind = ("reparse" if reparse else
                    "dir" if stat.S_ISDIR(info.st_mode) else
                    "file" if stat.S_ISREG(info.st_mode) else "other")
            entry = {"path": "." if path == root else path.relative_to(root).as_posix(),
                     "kind": kind, "bytes": info.st_size if kind == "file" else None}
            entries.append(entry)
            if kind == "dir" and depth < 3:
                children = list(islice(path.iterdir(), 81 - len(entries)))
                pending.extend((child, depth + 1) for child in children[:80 - len(entries)])
                truncated |= len(children) > 80 - len(entries)
            elif kind == "dir" and next(path.iterdir(), None) is not None:
                truncated = True
        except OSError as error:
            if entry is None:
                entries.append({"path": "." if path == root else path.relative_to(root).as_posix(),
                                "error": type(error).__name__})
            else:
                entry["error"] = type(error).__name__
    return {"exists": True, "entries": entries, "truncated": truncated or bool(pending)}


def assert_known_folders_unchanged(env, scratch, before, report, phase):
    current = known_folder_product_paths(env, scratch)
    report[f"known_folder_candidates_{phase}"] = current
    added = [name for name, existed in before.items() if not existed and current.get(name)]
    if current.keys() != before.keys() or added:
        try:
            locations = powershell_json(
                "@{roaming=[Environment]::GetFolderPath('ApplicationData');"
                "local=[Environment]::GetFolderPath('LocalApplicationData')}|ConvertTo-Json -Compress",
                env,
            )
            diagnostic = {"new_known_folders": {
                name: bounded_folder_metadata(Path(locations[name.split('/', 1)[0]]) /
                                              name.split('/', 1)[1]) for name in added},
                "repo_webview": bounded_folder_metadata(Path(env["WEBVIEW2_USER_DATA_FOLDER"]))}
            name = f"{phase}-known-folder-metadata.json"
            (scratch / "evidence" / name).write_text(json.dumps(diagnostic, indent=2) + "\n")
            report["known_folder_diagnostic"] = name
        except Exception as error:
            report["known_folder_diagnostic_error"] = type(error).__name__
        raise RuntimeError(f"product data appeared in an OS known folder outside test profile during {phase}")


def run_installer(path, destination, env, evidence, label):
    # NSIS requires /D= last and unquoted, including when its path has spaces.
    result = subprocess.run(
        f'"{path}" /S /D={destination}', executable=str(path), shell=False, env=env,
        capture_output=True, timeout=300, check=False,
    )
    (evidence / f"{label}-installer.stdout").write_bytes(result.stdout)
    (evidence / f"{label}-installer.stderr").write_bytes(result.stderr)
    if result.returncode:
        raise RuntimeError(f"{label} NSIS exit {result.returncode}")
    executable = destination / "voicetype-desktop.exe"
    if not executable.is_file():
        raise RuntimeError(f"{label} installed App executable missing")
    return executable


def assert_app_stopped(env, phase):
    deadline = time.monotonic() + 10
    while True:
        state = powershell_json(
            "$processes = @(Get-Process -Name 'voicetype-desktop' "
            "-ErrorAction SilentlyContinue); "
            "@{count=$processes.Count}|ConvertTo-Json -Compress",
            env,
        )
        if state["count"] == 0:
            return
        if time.monotonic() >= deadline:
            raise RuntimeError(f"installed App is still running after {phase}")
        time.sleep(0.2)


def uninstall(registry, destination, env, evidence, label):
    command = registry["uninstall"] or ""
    match = re.match(r'^\s*(?:"([^"]+\.exe)"|(.+?\.exe))(?:\s|$)', command, re.I)
    if not match:
        raise RuntimeError(f"{label} NSIS UninstallString is not an executable path")
    uninstaller = Path(match.group(1) or match.group(2)).resolve(strict=True)
    if not uninstaller.is_relative_to(destination) or not uninstaller.is_file():
        raise RuntimeError(f"{label} uninstaller is outside owned installation")
    result = subprocess.run(
        [str(uninstaller), "/S"], env=env, capture_output=True,
        timeout=300, check=False,
    )
    (evidence / f"{label}-uninstall.stdout").write_bytes(result.stdout)
    (evidence / f"{label}-uninstall.stderr").write_bytes(result.stderr)
    if result.returncode:
        raise RuntimeError(f"{label} NSIS uninstall exit {result.returncode}")
    deadline = time.monotonic() + 10
    while destination.exists() and any(destination.iterdir()):
        if time.monotonic() >= deadline:
            raise RuntimeError(f"{label} uninstall left installed product files")
        time.sleep(0.2)


class InstalledApp:
    def __init__(self, executable, env, evidence, label):
        self.executable = executable
        self.env = env
        self.evidence = evidence
        self.label = label
        self.driver = None
        self.session = None
        self.log = None

    def request(self, method, route, body=None, timeout=30):
        encoded = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(
            "http://127.0.0.1:4444" + route, data=encoded, method=method,
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return json.load(response).get("value")

    def __enter__(self):
        driver = shutil.which("tauri-driver")
        if not driver:
            raise RuntimeError("tauri-driver is required")
        self.log = (self.evidence / f"{self.label}-webdriver.log").open("wb")
        self.driver = subprocess.Popen([driver], env=self.env, stdout=self.log, stderr=self.log)
        try:
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                try:
                    if self.request("GET", "/status", timeout=2).get("ready"):
                        break
                except (urllib.error.URLError, http.client.RemoteDisconnected, TimeoutError):
                    pass
                time.sleep(0.2)
            else:
                raise RuntimeError("native WebDriver did not become ready")
            requested_udf = Path(self.env["WEBVIEW2_USER_DATA_FOLDER"])
            opened = self.request("POST", "/session", {"capabilities": {"alwaysMatch": {
                "browserName": "wry", "tauri:options": {
                    "application": str(self.executable),
                    "webviewOptions": {"userDataFolder": str(requested_udf)},
                }
            }}}, timeout=90)
            self.session = opened["sessionId"]
            reported_udf = opened.get("capabilities", {}).get("msedge.userDataDir")
            actual_udf = Path(reported_udf).resolve() if reported_udf else None
            expected_udf = requested_udf.resolve()
            matches = (actual_udf == expected_udf and requested_udf.is_dir())
            (self.evidence / f"{self.label}-webview-udf.json").write_text(json.dumps({
                "requested": str(expected_udf), "reported": str(actual_udf) if actual_udf else None,
                "requested_exists": requested_udf.is_dir(), "matches": matches,
            }, indent=2) + "\n", encoding="utf-8")
            if not matches:
                raise RuntimeError("WebDriver WebView2 user data folder is outside the test workspace")
            return self
        except Exception:
            self.__exit__(None, None, None)
            raise

    def invoke(self, name, args=None, audio=False):
        # W3C async script waits for the actual public Tauri command response.
        script = """
const done=arguments[arguments.length-1];
window.__TAURI__.core.invoke(arguments[0], arguments[1]).then(value => {
  if (arguments[2]) {
    const bytes = new Uint8Array(value);
    done({ok:true,value:{length:bytes.length,header:Array.from(bytes.slice(0,4))}});
  } else done({ok:true,value});
}, error => done({ok:false,error:String(error)}));
"""
        route = f"/session/{self.session}/execute/async"
        try:
            result = self.request("POST", route,
                                  {"script": script, "args": [name, args or {}, audio]}, timeout=60)
        except urllib.error.HTTPError as error:
            body = error.read(65537)
            diagnostic = self.evidence / f"{self.label}-webdriver-http-error.json"
            diagnostic.write_bytes(body[:65536])
            raise RuntimeError(
                f"WebDriver POST {route} returned HTTP {error.code}; "
                f"response saved to {diagnostic.name} "
                f"({min(len(body), 65536)} bytes, truncated={len(body) > 65536})"
            ) from error
        if not result["ok"]:
            raise RuntimeError(f"public App command {name} failed: {result['error']}")
        return result["value"]

    def __exit__(self, _kind, _value, _trace):
        try:
            if self.session:
                try:
                    self.request("DELETE", f"/session/{self.session}", timeout=20)
                finally:
                    self.session = None
        finally:
            if self.driver:
                self.driver.terminate()
                try:
                    self.driver.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    self.driver.kill()
                    self.driver.wait(timeout=10)
                self.driver = None
            if self.log:
                self.log.close()
                self.log = None


def stage_personal_data(app, profile):
    app.invoke("set_spelling_enabled", {"enabled": False})
    vocab = app.invoke("get_vocabulary")
    app.invoke("edit_vocabulary", {"revision": vocab["revision"], "change": {
        "kind": "put", "index": None, "wrong": ["測式"], "right": "測試",
    }})
    review = app.invoke("get_review")
    app.invoke("set_review_enabled", {
        "revision": review["settings"]["revision"], "enabled": True,
    })
    sample = profile / "review" / REVIEW_ID
    sample.mkdir(parents=True)
    record = {"version": 1, "id": REVIEW_ID, "created_at": int(time.time()),
              "duration_ms": 1000, "sample_rate": 16000, "status": "pending",
              "asr_text": "測式", "output_text": "測式", "corrected_text": None}
    (sample / "record.json").write_text(json.dumps(record, ensure_ascii=False), encoding="utf-8")
    with wave.open(str(sample / "audio.wav"), "wb") as output:
        output.setnchannels(1)
        output.setsampwidth(2)
        output.setframerate(16000)
        output.writeframes(b"\0\0" * 16000)
    learning = profile / "local-profile/data/learning.json"
    learning.parent.mkdir(parents=True)
    learning.write_text(json.dumps({"version": 1, "rules": [{
        "wrong": "測式", "right": "測試", "program": "synthetic-upgrade",
        "context_id": "fixture", "confirmed": True, "evidence": [],
    }]}, ensure_ascii=False), encoding="utf-8")


def snapshot(profile):
    return {name: digest(profile / name) for name in FILES}


def assert_public_data(app, profile, expected):
    settings = app.invoke("get_settings")
    if settings["settings"]["spelling_enabled"] is not False:
        raise RuntimeError("public settings lost the saved correction choice")
    vocab = app.invoke("get_vocabulary")
    if not any(item["wrong"] == ["測式"] and item["right"] == "測試"
               for item in vocab["entries"]):
        raise RuntimeError("public vocabulary did not reload the saved rule")
    review = app.invoke("get_review")
    if not review["settings"]["enabled"]:
        raise RuntimeError("public review setting was lost")
    item = next((item for item in review["items"] if item["id"] == REVIEW_ID), None)
    if not item or item["status"] != "pending":
        raise RuntimeError("public review item is missing")
    audio = app.invoke("get_review_audio", {
        "id": item["id"], "revision": item["revision"],
    }, audio=True)
    if audio != {"length": 32044, "header": [82, 73, 70, 70]}:
        raise RuntimeError("public review audio did not return the synthetic WAV")
    if snapshot(profile) != expected:
        raise RuntimeError("personal data changed while public commands reloaded it")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for generation in ("old", "new"):
        parser.add_argument(f"--{generation}-installer", required=True)
        parser.add_argument(f"--{generation}-sha256", required=True)
        parser.add_argument(f"--{generation}-version", required=True)
    args = parser.parse_args()
    if (os.name != "nt" or os.environ.get("RUNNER_OS") != "Windows"
            or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted"
            or not os.environ.get("GITHUB_ACTIONS")):
        raise RuntimeError("this installer test requires a disposable GitHub-hosted Windows runner")
    workspace = Path(os.environ["GITHUB_WORKSPACE"]).resolve(strict=True)
    old = require_within(args.old_installer, workspace)
    new = require_within(args.new_installer, workspace)
    if old == new or args.old_version == args.new_version:
        raise RuntimeError("two distinct installer files and versions are required")
    for installer, expected in ((old, args.old_sha256), (new, args.new_sha256)):
        if not re.fullmatch(r"[a-f0-9]{64}", expected) or digest(installer) != expected:
            raise RuntimeError(f"installer SHA256 mismatch: {installer.name}")
    scratch = workspace / ".scratch/windows-upgrade-smoke"
    scratch.mkdir(parents=True, exist_ok=False)
    evidence = scratch / "evidence"
    evidence.mkdir()
    profile = scratch / "profile"
    destination = scratch / "install 語音"
    appdata = scratch / "AppData/Roaming"
    localappdata = scratch / "AppData/Local"
    temporary = scratch / "temp"
    for directory in (profile, appdata, localappdata, temporary):
        directory.mkdir(parents=True)
    env = os.environ.copy()
    env.update({"VOICETYPE_PREVIEW_CONFIG_DIR": str(profile),
                "APPDATA": str(appdata), "LOCALAPPDATA": str(localappdata),
                "TEMP": str(temporary), "TMP": str(temporary),
                "WEBVIEW2_USER_DATA_FOLDER": str(scratch / "webview2")})
    report = {"old_installer_sha256": digest(old), "new_installer_sha256": digest(new),
              "old_expected_version": args.old_version,
              "new_expected_version": args.new_version, "phases": []}
    try:
        if product_registration_count(env):
            raise RuntimeError("runner already has a VoiceType Preview registration")
        known_before = known_folder_product_paths(env, scratch)
        report["known_folder_candidates_before"] = known_before
        old_setup_version, old_setup_info = file_version(old, env, args.old_version)
        new_setup_version, new_setup_info = file_version(new, env, args.new_version)
        report["setup_pe_versions"] = {"old": old_setup_info, "new": new_setup_info}
        if not old_setup_version < new_setup_version:
            raise RuntimeError("NSIS setup numeric PE version did not increase")
        old_exe = run_installer(old, destination, env, evidence, "old-install")
        old_registry = registration(destination, env)
        if old_registry["display"] != args.old_version:
            raise RuntimeError("old NSIS registered version differs from expected")
        old_app_version, old_app_info = file_version(old_exe, env, args.old_version)
        report["old_app_pe_version"] = old_app_info
        report["old_app_sha256"] = digest(old_exe)
        if old_app_version != old_setup_version:
            raise RuntimeError("old NSIS setup and installed App PE versions disagree")
        with InstalledApp(old_exe, env, evidence, "old") as app:
            stage_personal_data(app, profile)
            saved = snapshot(profile)
            assert_public_data(app, profile, saved)
        assert_app_stopped(env, "old data read")
        assert_known_folders_unchanged(env, scratch, known_before, report, "old")
        report["phases"].append("old-installed-public-data")
        new_exe = run_installer(new, destination, env, evidence, "new-upgrade")
        new_registry = registration(destination, env)
        if new_registry["display"] != args.new_version:
            raise RuntimeError("new NSIS registered version differs from expected")
        new_app_version, new_app_info = file_version(new_exe, env, args.new_version)
        report["new_app_pe_version"] = new_app_info
        report["new_app_sha256"] = digest(new_exe)
        if new_app_version != new_setup_version:
            raise RuntimeError("new NSIS setup and installed App PE versions disagree")
        if not old_app_version < new_app_version:
            raise RuntimeError("installed App numeric PE version did not increase")
        if report["old_app_sha256"] == report["new_app_sha256"]:
            raise RuntimeError("in-place upgrade left the old App executable unchanged")
        with InstalledApp(new_exe, env, evidence, "upgraded") as app:
            assert_public_data(app, profile, saved)
        assert_app_stopped(env, "new data read")
        assert_known_folders_unchanged(env, scratch, known_before, report, "upgrade")
        report["phases"].append("new-installed-public-data")
        uninstall(new_registry, destination, env, evidence, "new")
        if snapshot(profile) != saved:
            raise RuntimeError("new uninstall changed personal data")
        if product_registration_count(env):
            raise RuntimeError("new NSIS registration remains after uninstall")
        report["phases"].append("new-uninstalled-data-retained")
        restored_exe = run_installer(old, destination, env, evidence, "old-rollback")
        restored_registry = registration(destination, env)
        restored_version, _ = file_version(restored_exe, env, args.old_version)
        if restored_registry["display"] != args.old_version or restored_version != old_app_version:
            raise RuntimeError("old installer was not restored after uninstall")
        with InstalledApp(restored_exe, env, evidence, "rollback") as app:
            assert_public_data(app, profile, saved)
        assert_app_stopped(env, "rollback data read")
        assert_known_folders_unchanged(env, scratch, known_before, report, "rollback")
        report["phases"].append("old-restored-public-data")
        uninstall(restored_registry, destination, env, evidence, "old")
        if snapshot(profile) != saved:
            raise RuntimeError("final uninstall changed personal data")
        if product_registration_count(env):
            raise RuntimeError("NSIS registration remains after final uninstall")
        assert_known_folders_unchanged(env, scratch, known_before, report, "after_uninstall")
        report["phases"].append("final-uninstall-data-retained")
        report["personal_sha256"] = saved
        report["result"] = "passed"
    except Exception as error:
        report["result"] = "failed"
        report["failure"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        (evidence / "upgrade-report.json").write_text(
            json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
