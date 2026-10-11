"""Read only the pinned official Windows CLI parser help on a hosted runner.

Usage: python windows_google_cli_help.py --binary PATH_TO_AGY_EXE
This never invokes a voice command, signs in, or tests PCM/stop semantics.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


PINNED_SHA256 = "38f30c7dd1ed808f5cf98fe2014de3d30903035a4f0df02d3eb72a9ff8993741"
MAX_HELP_BYTES = 16 * 1024
HELP_TIMEOUT_SECONDS = 20


class ProbeFailure(RuntimeError):
    """Keep the repo-owned profile for bounded help-probe diagnostics."""


class CleanupUnconfirmed(ProbeFailure):
    """The timed-out CLI may still own descendants in the isolated profile."""


def digest(path):
    sha = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            sha.update(chunk)
    return sha.hexdigest()


def run_help(binary, args, cwd, env, label):
    stdout_path = cwd / f"{label}.stdout"
    stderr_path = cwd / f"{label}.stderr"
    with stdout_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
        child = subprocess.Popen([str(binary), *args], cwd=cwd, env=env,
                                 stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr,
                                 creationflags=subprocess.CREATE_NEW_PROCESS_GROUP)
        try:
            code = child.wait(timeout=HELP_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired as error:
            # Terminate the CLI and any children before discarding this profile.
            tree_killed = False
            try:
                result = subprocess.run(
                    [str(Path(env["SystemRoot"]) / "System32/taskkill.exe"),
                     "/PID", str(child.pid), "/T", "/F"],
                    env=env, cwd=cwd, stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                    timeout=5, check=False)
                tree_killed = result.returncode == 0
            except (OSError, subprocess.TimeoutExpired):
                pass
            finally:
                if child.poll() is None:
                    try:
                        child.kill()
                    except OSError as kill_error:
                        raise CleanupUnconfirmed(
                            f"{label} help timed out; direct-child kill failed"
                        ) from kill_error
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired as wait_error:
                    raise CleanupUnconfirmed(
                        f"{label} help timed out; child exit unconfirmed"
                    ) from wait_error
            if not tree_killed:
                raise CleanupUnconfirmed(
                    f"{label} help timed out; process-tree cleanup unconfirmed"
                ) from error
            raise ProbeFailure(
                f"{label} help timed out; taskkill /T succeeded and child exit was observed"
            ) from error
    # This caps the published artifact after exit, not the child's write rate.
    if stdout_path.stat().st_size + stderr_path.stat().st_size > MAX_HELP_BYTES:
        raise ProbeFailure(f"{label} help exceeded {MAX_HELP_BYTES} bytes")
    output = stdout_path.read_text(encoding="utf-8", errors="replace")
    output += stderr_path.read_text(encoding="utf-8", errors="replace")
    if code != 0 or "usage" not in output.casefold():
        raise ProbeFailure(f"{label} did not return parser help (exit {code})")
    return {"exit_code": code, "help": output}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    args = parser.parse_args()
    if (os.name != "nt" or os.environ.get("GITHUB_ACTIONS") != "true"
            or os.environ.get("RUNNER_OS") != "Windows"
            or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted"):
        raise RuntimeError("help probe requires a disposable GitHub-hosted Windows runner")
    workspace = Path(os.environ["GITHUB_WORKSPACE"]).resolve(strict=True)
    supplied = Path(args.binary)
    binary = supplied.resolve(strict=True)
    if (not binary.is_relative_to(workspace) or binary.name.lower() != "agy.exe"
            or supplied.is_symlink() or not binary.is_file()):
        raise RuntimeError("expected a repo-owned, regular agy.exe")
    if digest(binary) != PINNED_SHA256:
        raise RuntimeError("official Windows 1.3.1 image SHA256 differs")

    scratch = workspace / ".scratch" / "google-cli-help"
    scratch.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="probe-", dir=scratch))
    cleanup_confirmed = True
    try:
        for name in ("profile", "appdata", "localappdata", "temp", "config", "data"):
            (root / name).mkdir()
        system_root = os.environ["SystemRoot"]
        env = {
            "SystemRoot": system_root, "WINDIR": system_root,
            "PATH": str(Path(system_root) / "System32"),
            "USERPROFILE": str(root / "profile"),
            "APPDATA": str(root / "appdata"),
            "LOCALAPPDATA": str(root / "localappdata"),
            "TEMP": str(root / "temp"), "TMP": str(root / "temp"),
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "AGY_CLI_DISABLE_AUTO_UPDATE": "true", "NO_COLOR": "1", "CI": "1",
        }
        top = run_help(binary, ("--help",), root, env, "top-level")
        if "mic-serve" not in top["help"].casefold():
            raise ProbeFailure("top-level help does not advertise mic-serve; subcommand not run")
        mic = run_help(binary, ("mic-serve", "--help"), root, env, "mic-serve")
        if (mic["help"] == top["help"]
                or not re.search(r"(?im)^\s*usage\s*:[^\r\n]*\bmic-serve\b", mic["help"])
                or "--addr" not in mic["help"]):
            raise ProbeFailure("mic-serve did not return subcommand-specific parser help")
        if digest(binary) != PINNED_SHA256:
            raise RuntimeError("official CLI image changed during help probe")
        print(json.dumps({
            "binary_sha256": PINNED_SHA256, "version_pin": "1.3.1",
            "top_level": top, "mic_serve": mic,
            "unknown": ["Windows microphone selection", "stream framing/PCM format",
                        "consumer ACK", "EOF/flush", "stop drain", "sample equivalence"],
        }, ensure_ascii=False))
    except ProbeFailure as error:
        cleanup_confirmed = False
        raise ProbeFailure(
            f"{error}; isolated profile retained at {root}"
        ) from error
    finally:
        if cleanup_confirmed:
            shutil.rmtree(root)


if __name__ == "__main__":
    main()
