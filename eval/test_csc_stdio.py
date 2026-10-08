#!/usr/bin/env python3
"""Cross-platform owned-process boundary. Fixture engine, no model or audio."""
import json
import queue
import threading
from pathlib import Path
import subprocess
import sys
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))


def worker():
    from voicetype_csc import serve_stdio

    class Engine:
        def predict(self, text, terms, budget_ms):
            self.last_calls = 1
            if text == "private failure text":
                raise RuntimeError(text)
            if text == "今天新情很好。" and "新情" not in terms:
                return [dict(start=2, source="新", target="心")], "applied"
            return [], "unchanged"

    serve_stdio(Engine())


class StdioTests(unittest.TestCase):
    def test_owned_pipe_returns_matching_edits_then_exits_on_parent_eof(self):
        process = subprocess.Popen([sys.executable, __file__, "--worker"],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        ready = queue.Queue()
        threading.Thread(target=lambda: ready.put(process.stdout.readline()), daemon=True).start()
        try:
            first = ready.get(timeout=3)
            self.assertEqual(json.loads(first), dict(v=1, status="ready", provider="CPUExecutionProvider"))
            request = dict(v=1, id=21, text="今天新情很好。", terms=[], sent_at_ms=int(time.time()*1000))
            frames = [request, dict(request, id=22, terms=["新情"])]
            stdout, stderr = process.communicate(b"".join(json.dumps(r).encode()+b"\n" for r in frames), timeout=3)
            self.assertEqual(process.returncode, 0, stderr.decode(errors="replace"))
            replies = [json.loads(line) for line in stdout.splitlines()]
            self.assertEqual(replies[0], dict(v=1, id=21, status="applied", model_calls=1,
                                            edits=[dict(start=2, source="新", target="心")]))
            self.assertEqual(replies[1]["id"], 22)
            self.assertEqual(replies[1]["edits"], [])
            self.assertNotIn("今天新情很好", stdout.decode())
            self.assertEqual(stderr, b"")
        finally:
            if process.poll() is None:
                process.kill()
            process.communicate(timeout=3)



    def test_bad_frames_exit_without_echoing_transcripts_and_old_requests_skip_inference(self):
        samples = [b'{"text":"private malformed text"}\n', b"[]\n", b"\xff\n", b"x" * 65537,
                   b'{"private":"truncated"}',
                   json.dumps(dict(v=1, id=1, text="private failure text", terms=[], sent_at_ms=int(time.time()*1000)+10000)).encode()+b"\n"]
        for payload in samples:
            result = subprocess.run([sys.executable, __file__, "--worker"], input=payload,
                                    capture_output=True, timeout=3)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(len(result.stdout.splitlines()), 1)
            self.assertEqual(result.stderr, b"spelling pipe request failed\n")
        payload = json.dumps(dict(v=1, id=9, text="今天新情很好。", terms=[], sent_at_ms=1)).encode()+b"\n"
        result = subprocess.run([sys.executable, __file__, "--worker"], input=payload,
                                capture_output=True, timeout=3)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(json.loads(result.stdout.splitlines()[1]),
                         dict(v=1, id=9, status="deadline", model_calls=0, edits=[]))


if __name__ == "__main__":
    if sys.argv[1:] == ["--worker"]:
        worker()
    else:
        unittest.main()
