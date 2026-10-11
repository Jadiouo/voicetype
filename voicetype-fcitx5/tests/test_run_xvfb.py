"""No real X server: exercise only the displayfd startup and cleanup protocol."""
import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest import mock


WRAPPER = Path(__file__).with_name("run_xvfb.py")
spec = importlib.util.spec_from_file_location("run_xvfb", WRAPPER)
wrapper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wrapper)

FAKE_SERVER = '''#!/usr/bin/env python3
import os, sys, time
fd = int(sys.argv[sys.argv.index('-displayfd') + 1])
with open(os.environ['VT_FAKE_PIDFILE'], 'w') as record:
    record.write(str(os.getpid()))
mode = os.environ['VT_FAKE_MODE']
if mode == 'exit':
    os.write(2, b'fixture exited before displayfd\\n')
    sys.exit(23)
if mode == 'huge':
    os.write(2, b'A' * 6000)
    sys.exit(29)
if mode == 'eof':
    os.close(fd)
else:
    if mode == 'ready':
        os.write(fd, b'87\\n')
    else:
        os.write(2, b'fixture remains unready\\n')
time.sleep(30)
'''


class XvfbStartupTests(unittest.TestCase):
    def setUp(self):
        scratch = Path(os.environ.get("VOICETYPE_TEST_TMPDIR", Path(__file__).resolve().parents[2] / ".scratch" / "xvfb-fixture"))
        scratch.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.fake = self.root / "fake_xvfb.py"
        self.fake.write_text(FAKE_SERVER)
        self.fake.chmod(0o755)
        self.pidfile = self.root / "pid"

    def launch(self, mode, timeout=0.3):
        with mock.patch.dict(os.environ, VT_FAKE_MODE=mode, VT_FAKE_PIDFILE=str(self.pidfile)):
            return wrapper.start_xvfb([str(self.fake)], self.root, timeout)

    def assert_reaped(self):
        pid = int(self.pidfile.read_text())
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)

    def test_early_exit_reports_status_and_bounded_stderr(self):
        started = time.monotonic()
        with self.assertRaisesRegex(RuntimeError, r'exited.*exit=23.*fixture exited'):
            self.launch('exit', timeout=0.6)
        self.assertLess(time.monotonic() - started, 2)
        self.assert_reaped()

    def test_stderr_excerpt_is_capped(self):
        with self.assertRaises(RuntimeError) as caught:
            self.launch('huge', timeout=0.6)
        self.assertIn('exit=29', str(caught.exception))
        self.assertIn('[truncated]', str(caught.exception))
        self.assertLess(len(str(caught.exception).split("stderr=", 1)[1]), 4120)
        self.assert_reaped()

    def test_unready_timeout_reports_elapsed_and_reaps(self):
        with self.assertRaisesRegex(RuntimeError, r'Xvfb startup timeout.*elapsed=.*command=.*fixture remains unready'):
            self.launch('idle', timeout=0.2)
        self.assert_reaped()

    def test_displayfd_eof_fails_before_deadline_and_reaps(self):
        started = time.monotonic()
        with self.assertRaisesRegex(RuntimeError, r'displayfd EOF'):
            self.launch('eof', timeout=0.6)
        self.assertLess(time.monotonic() - started, 0.5)
        self.assert_reaped()

    def test_ready_display_has_owner_until_explicit_cleanup(self):
        server, stderr, display = self.launch('ready', timeout=0.6)
        self.assertEqual(display, ':87')
        self.assertIsNone(server.poll())
        try:
            wrapper.stop_xvfb(server)
        finally:
            stderr.close()
        self.assert_reaped()


if __name__ == '__main__':
    unittest.main()
