#!/usr/bin/env python3
"""Launch tests only in our own X server; never inject into inherited DISPLAY."""

import os
from pathlib import Path
import select
import shlex
import signal
import subprocess
import sys
import tempfile
import time


STARTUP_TIMEOUT = 5  # Test X-server readiness, not a dictation latency limit.
STDERR_LIMIT = 4096


def stop_xvfb(server):
    try:
        os.killpg(server.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        server.wait(timeout=3)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(server.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        server.wait(timeout=3)


def _stderr_excerpt(stderr_file):
    size = os.fstat(stderr_file.fileno()).st_size
    stderr_file.seek(0)
    excerpt = stderr_file.read(STDERR_LIMIT).decode("utf-8", errors="replace").strip()
    return excerpt + (" [truncated]" if size > STDERR_LIMIT else "")


def start_xvfb(xvfb_command, temp, timeout=STARTUP_TIMEOUT):
    stderr_file = tempfile.TemporaryFile(mode="w+b", dir=temp)
    try:
        rd, wr = os.pipe()
    except BaseException:
        stderr_file.close()
        raise
    command = [*xvfb_command, "-displayfd", str(wr), "-screen", "0", "800x600x24", "-nolisten", "tcp"]
    started = time.monotonic()
    try:
        server = subprocess.Popen(
            command,
            pass_fds=[wr],
            stdout=subprocess.DEVNULL,
            stderr=stderr_file,
            start_new_session=True,
        )
    except BaseException:
        os.close(rd)
        os.close(wr)
        stderr_file.close()
        raise
    os.close(wr)

    def failure(reason):
        elapsed = time.monotonic() - started
        return RuntimeError(
            f"{reason}; elapsed={elapsed:.3f}s; command={shlex.join(command)}; "
            f"stderr={_stderr_excerpt(stderr_file)!r}"
        )

    try:
        received = bytearray()
        while True:
            status = server.poll()
            if status is not None:
                raise failure(f"Xvfb exited before displayfd (exit={status})")
            remaining = started + timeout - time.monotonic()
            if remaining <= 0:
                raise failure("Xvfb startup timeout")
            if not select.select([rd], [], [], min(remaining, 0.1))[0]:
                continue
            chunk = os.read(rd, 64 - len(received))
            if not chunk:
                try:
                    status = server.wait(timeout=0.05)
                except subprocess.TimeoutExpired:
                    raise failure("Xvfb displayfd EOF") from None
                raise failure(f"Xvfb exited before displayfd (exit={status})")
            received.extend(chunk)
            if b"\n" in received:
                number = received.decode("ascii", errors="replace").strip()
                if not number.isdecimal():
                    raise failure("invalid isolated display")
                return server, stderr_file, ":" + number
            if len(received) == 64:
                raise failure("invalid isolated display")
    except BaseException as error:
        try:
            stop_xvfb(server)
        except BaseException as cleanup_error:
            error.add_note(f"Xvfb cleanup failed: {cleanup_error}")
        stderr_file.close()
        raise
    finally:
        os.close(rd)


def main():
    xvfb, executable, *args = sys.argv[1:]
    scratch = Path(os.environ.get("VOICETYPE_TEST_TMPDIR", Path(__file__).resolve().parents[2] / ".scratch" / "fcitx-xvfb"))
    scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="voicetype-caps-test-", dir=scratch) as temp:
        server, stderr_file, display = start_xvfb([xvfb], temp)
        try:
            if display == os.environ.get("DISPLAY"):
                raise RuntimeError("refusing user display")
            env = os.environ.copy()
            env.update(
                DISPLAY=display,
                XDG_RUNTIME_DIR=temp,
                FCITX_CONFIG_HOME=temp,
                FCITX_DATA_HOME=temp,
                VOICETYPE_ISOLATED_DISPLAY="1",
                VOICETYPE_SOCKET=temp + "/absent.sock",
            )
            subprocess.run(["setxkbmap", "-display", display, "-layout", "us", "-option", ""], env=env, check=True)
            result = subprocess.run([executable, *args], env=env, timeout=15)
        except BaseException as error:
            try:
                stop_xvfb(server)
            except BaseException as cleanup_error:
                error.add_note(f"Xvfb cleanup failed: {cleanup_error}")
            raise
        else:
            stop_xvfb(server)
            return result.returncode
        finally:
            stderr_file.close()


if __name__ == "__main__":
    raise SystemExit(main())
