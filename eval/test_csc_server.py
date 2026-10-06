#!/usr/bin/env python3
"""Real Unix-socket worker tests with no weights, microphone, or desktop input."""
import contextlib
import json
import multiprocessing
import os
from pathlib import Path
import signal
import socket
import sys
import tempfile
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from voicetype_csc import serve


class FakeEngine:
    def predict(self, text, terms, budget_ms):
        self.last_calls = 1
        if text == "raise without logging this transcript":
            raise RuntimeError(text)
        if text == "slow inference":
            time.sleep(.15)
        if text == "今天新情很好。" and "新情" not in terms:
            return [dict(start=2, source="新", target="心")], "applied"
        return [], "unchanged"


def launch(path, log):
    with open(log, "w", buffering=1) as stream:
        with contextlib.redirect_stdout(stream), contextlib.redirect_stderr(stream):
            serve(FakeEngine(), path)


class ServerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="voicetype-csc-test-")
        self.root = Path(self.temp.name)
        self.path = self.root / "worker.sock"
        self.log = self.root / "worker.log"
        self.worker = multiprocessing.get_context("fork").Process(
            target=launch, args=(self.path, self.log))
        self.worker.start()
        until = time.monotonic() + 3
        while not self.path.exists() and time.monotonic() < until:
            if not self.worker.is_alive():
                self.fail(self.log.read_text())
            time.sleep(.005)
        self.assertTrue(self.path.exists())

    def tearDown(self):
        if self.worker.is_alive():
            os.kill(self.worker.pid, signal.SIGTERM)
        self.worker.join(2)
        if self.worker.is_alive():
            self.worker.kill()
            self.worker.join()
        self.temp.cleanup()

    def connect(self):
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(.7)
        client.connect(str(self.path))
        return client

    def payload(self, text="今天新情很好。", **values):
        request = dict(v=1, id=42, text=text, terms=[], sent_at_ms=int(time.time()*1000))
        request.update(values)
        return json.dumps(request, ensure_ascii=False).encode() + b"\n"

    def exchange(self, payload):
        with self.connect() as client:
            client.sendall(payload)
            data = bytearray()
            while b"\n" not in data:
                part = client.recv(4096)
                if not part:
                    return None
                data.extend(part)
            return json.loads(data.split(b"\n", 1)[0])

    def test_correction_terms_and_private_socket(self):
        reply = self.exchange(self.payload())
        self.assertEqual(reply, dict(v=1, id=42, status="applied", model_calls=1,
                                    edits=[dict(start=2, source="新", target="心")]))
        self.assertEqual(self.exchange(self.payload(terms=["新情"]))["edits"], [])
        self.assertEqual(self.path.stat().st_mode & 0o777, 0o600)

    def test_stale_request_skips_actual_inference(self):
        reply = self.exchange(self.payload(sent_at_ms=int(time.time()*1000)-500))
        self.assertEqual(reply["status"], "deadline")
        self.assertEqual(reply["model_calls"], 0)
        self.assertEqual(reply["edits"], [])

    def test_malformed_requests_do_not_stop_worker(self):
        invalid = [b"[]\n", b"{bad}\n", b"\xff\n", self.payload(v=True),
                   self.payload(id=True), self.payload(terms=[3]),
                   self.payload(text="x"*4097)]
        for data in invalid:
            with self.subTest(data=data[:32]):
                self.assertIsNone(self.exchange(data))
        self.assertEqual(self.exchange(self.payload())["status"], "applied")

    def test_slow_drip_uses_one_absolute_read_deadline(self):
        with self.connect() as client:
            started = time.monotonic()
            for _ in range(20):
                try:
                    client.sendall(b" ")
                except (BrokenPipeError, ConnectionResetError):
                    break
                time.sleep(.012)
            self.assertLess(time.monotonic()-started, .2)
        self.assertEqual(self.exchange(self.payload())["status"], "applied")

    def test_disconnected_slow_inference_recovers_and_drops_stale_queue(self):
        with self.connect() as client:
            client.sendall(self.payload("slow inference"))
        time.sleep(.01)
        reply = self.exchange(self.payload())
        self.assertEqual(reply["status"], "deadline")
        self.assertEqual(reply["model_calls"], 0)
        self.assertEqual(self.exchange(self.payload())["status"], "applied")

    def test_errors_and_logs_do_not_include_transcripts(self):
        secret = "raise without logging this transcript"
        self.assertIsNone(self.exchange(self.payload(secret)))
        self.assertEqual(self.exchange(self.payload())["status"], "applied")
        logged = self.log.read_text()
        self.assertNotIn(secret, logged)
        self.assertNotIn("新情", logged)
        self.assertIn("inference_failed", logged)

    def test_shutdown_removes_only_owned_socket(self):
        os.kill(self.worker.pid, signal.SIGTERM)
        self.worker.join(2)
        self.assertEqual(self.worker.exitcode, 0)
        self.assertFalse(self.path.exists())


if __name__ == "__main__":
    unittest.main()
