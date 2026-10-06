"""Recorder contracts tested with generated tones, never the microphone."""
import json
import math
import shutil
import subprocess
from pathlib import Path
import struct
import tempfile
import threading
import unittest
from unittest.mock import patch
from urllib.error import HTTPError
from urllib.request import Request, urlopen
import wave

from record_personal import DEFAULT_CORPUS, Recorder, ThreadingHTTPServer, audio_quality, handler_for


def tone(path, silent=False):
    with wave.open(str(path), "wb") as wav:
        wav.setparams((1, 2, 16000, 0, "NONE", "not compressed"))
        wav.writeframes(struct.pack("<16000h", *(0 if silent else int(2000 * math.sin(i / 10)) for i in range(16000))))


class RecorderTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.output = Path(self.temp.name)
        self.rec = Recorder(DEFAULT_CORPUS, self.output)

    def tearDown(self):
        self.rec.close()
        self.temp.cleanup()

    def make_take(self, cid="p001"):
        tone(self.rec.take)
        self.rec.current = cid
        self.rec.quality = audio_quality(self.rec.take)

    def test_silence_rejected_and_normal_pcm_measured(self):
        tone(self.rec.take, silent=True)
        with self.assertRaisesRegex(ValueError, "沒有聲音"):
            audio_quality(self.rec.take)
        tone(self.rec.take)
        self.assertEqual(audio_quality(self.rec.take)["seconds"], 1)

    def test_accept_requires_confirmation_and_resumes_verified_recording(self):
        self.make_take()
        with self.assertRaises(ValueError):
            self.rec.accept("p001", False)
        self.rec.accept("p001", True)
        self.assertEqual(Recorder(DEFAULT_CORPUS, self.output).progress(), ["p001"])
        data = json.loads((self.output / "p001.json").read_text())
        self.assertTrue(data["confirmed_reading"])
        self.assertEqual(data["reference"], self.rec.cases["p001"]["reference"])
        self.assertEqual((self.output / "p001.wav").stat().st_mode & 0o777, 0o600)

    def test_tampered_audio_not_marked_done(self):
        self.make_take()
        self.rec.accept("p001", True)
        with (self.output / "p001.wav").open("ab") as out:
            out.write(b"changed")
        self.assertEqual(self.rec.progress(), [])

    def test_retake_does_not_replace_existing_until_confirmed(self):
        self.make_take()
        self.rec.accept("p001", True)
        original = (self.output / "p001.wav").read_bytes()
        self.make_take()
        self.assertEqual((self.output / "p001.wav").read_bytes(), original)
        with self.assertRaises(ValueError):
            self.rec.accept("p002", True)

    def test_corpus_change_refused(self):
        frozen = self.output / "corpus.json"
        frozen.write_text(frozen.read_text() + "\n")
        with self.assertRaisesRegex(ValueError, "題目已變更"):
            Recorder(DEFAULT_CORPUS, self.output)

    def test_unauthorized_or_cross_origin_cannot_start_microphone(self):
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler_for(self.rec, "test-token"))
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        base = f"http://127.0.0.1:{server.server_port}"
        try:
            with patch("record_personal.subprocess.Popen") as spawn:
                for headers in ({}, {"X-Recorder-Token": "test-token", "Origin": "https://other.invalid"}):
                    with self.assertRaises(HTTPError) as err:
                        urlopen(Request(base + "/api/start", data=b'{"id":"p001"}', headers=headers))
                    self.assertEqual(err.exception.code, 403)
                spawn.assert_not_called()
            with urlopen(Request(base + "/api/state", headers={"X-Recorder-Token": "test-token"})) as response:
                self.assertEqual(len(json.load(response)["cases"]), 100)
        finally:
            server.shutdown()
            server.server_close()

    def run_ui(self, scenario):
        if not shutil.which("node"):
            self.skipTest("Node is required for the synthetic UI recovery checks")
        script = (Path(__file__).with_name("record_personal.html").read_text()
                  .split("<script>", 1)[1].split("</script>", 1)[0])
        harness = r"""
const assert = require('node:assert/strict');
const vm = require('node:vm');
const elements = new Map();
const element = () => ({textContent:'', hidden:false, disabled:false, checked:false,
  value:'', replaceChildren(){}, pause(){}, load(){}, removeAttribute(){}});
const cases = Array.from({length:100},(_,i)=>({id:`p${String(i+1).padStart(3,'0')}`,
  reference:'測試句子',context:'',notes:'',category:'daily_zh'}));
let serverState = {cases,done:[],recording:true,current:'p001',quality:null,error:null};
const intervals = new Map();
let intervalId=0;
const calls=[];
const box={
  document:{getElementById(id){if(!elements.has(id))elements.set(id,element());return elements.get(id)},
            createElement:element},
  window:{addEventListener(){}},
  setInterval(fn){intervals.set(++intervalId,fn);return intervalId},
  clearInterval(id){intervals.delete(id)},
  fetch:async(path,opts)=>{
    calls.push({path,method:opts.method});
    if(path==='/api/stop'){
      serverState={...serverState,recording:false,error:'錄音太短，請重錄'};
      return {ok:false,json:async()=>({error:'錄音太短，請重錄'})};
    }
    return {ok:true,json:async()=>JSON.parse(JSON.stringify(serverState))};
  }, Date, Error, console,
};
vm.createContext(box);
"""
        footer = r"""
(async()=>{
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(elements.get('stop').disabled,false);
  // Loading or polling the page must never call an endpoint that opens the mic.
  assert.ok(calls.every(c=>c.method==='GET'));
""" + scenario + r"""
})().catch(err=>{console.error(err);process.exitCode=1});
"""
        source = harness + "\nvm.runInContext(" + json.dumps(script) + ",box);\n" + footer
        result = subprocess.run(["node", "-e", source], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_ui_renders_authoritative_state_after_stop_error(self):
        self.run_ui(r"""
  await elements.get('stop').onclick();
  assert.equal(elements.get('start').disabled,false);
  assert.equal(elements.get('stop').disabled,true);
  assert.ok(!elements.get('status').textContent.includes('錄音中'));
  assert.ok(elements.get('error').textContent.includes('錄音太短'));
""")

    def test_ui_reload_observes_backend_auto_stop_without_starting_mic(self):
        self.run_ui(r"""
  serverState={...serverState,recording:false,quality:{seconds:45,clipped_fraction:0}};
  assert.ok(intervals.size>0,'An already-recording page must keep observing auto-stop');
  for(const poll of [...intervals.values()])await poll();
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(elements.get('review').hidden,false);
  assert.equal(elements.get('start').disabled,false);
  assert.equal(elements.get('stop').disabled,true);
  assert.ok(elements.get('status').textContent.includes('回放'));
  assert.ok(calls.every(c=>c.method==='GET'));
""")

    def test_stop_finalizes_without_auto_accepting(self):
        self.make_take()
        class FakeProcess:
            def poll(self): return 0
            def wait(self, timeout=None): return 0
        self.rec.proc = FakeProcess()
        self.rec.log = open(self.output / ".arecord.log", "wb")
        state = self.rec.state()
        self.assertFalse(state["recording"])
        self.assertEqual(state["done"], [])
        self.assertEqual(state["quality"]["seconds"], 1)


if __name__ == "__main__":
    unittest.main()
