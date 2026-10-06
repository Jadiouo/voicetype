# Local evaluation tools

`fixtures/dictation-demo.json` contains 100 generated, generic demo sentences,
split into 60 development and 40 test entries. It contains no recordings or
personal transcripts and is intended for exercising the recorder and scoring
tools, not measuring general recognition quality. Supply your own frozen corpus
with `--corpus` for meaningful evaluation.

```sh
python3 eval/record_personal.py --corpus eval/fixtures/dictation-demo.json
```

The authenticated loopback page starts the microphone only when the user presses
Record. Confirmation is required before a take is retained. Files and corpus
hashes are checked on resume. Recordings live under ignored `eval/recordings/`.

`evaluate_personal.py` accepts a daemon or Whisper executable, explicit model,
corpus and recordings paths. It verifies all required confirmed recordings before
running inference and reports incomplete runs without an aggregate score.
`replay_learning.py` and `adversarial_learning.py` use isolated learning stores;
references are exposed only at the subsequent feedback step. See each script's
`--help` for options. All generated reports should go in ignored `private/`.

`benchmark_csc.py`, `evaluate_csc.py`, `check_csc.py` and
`check_fast_autocorrect.py` separate text correction from audio recognition.
The last helper expects the example vocabulary; use an isolated daemon/socket
instead of overwriting a personal dictionary. `benchmark_text_model.py` is an
experimental local generation comparison, not part of the dictation service.
CPU is explicit; iGPU mode requires the user's PCI/device/render/Vulkan mapping
and verifies the opened device. Schedule GPU work through your shared queue
(`gpujob` where available); no GPU benchmark runs as part of these unit tests.

Install the Python evaluation dependencies from `config/csc-requirements.txt`
in an isolated environment, then run:

```sh
python3 -m unittest discover -s eval -p 'test_*.py'
python3 -m unittest discover -s scripts -p 'test_voicetype_*.py'
```

The settings tests additionally need `scripts/requirements-settings.txt` and
the OpenCC system library. Recorder tests generate tones and mock microphone
startup. Native addon tests run in their own synthetic contexts/Xvfb.
