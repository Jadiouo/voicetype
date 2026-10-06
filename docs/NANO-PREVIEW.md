# Optional native Nano CPU profile

The default compiled ASR remains SenseVoice. `sherpa-nano` adds an optional,
single-resident FunASR Nano recognizer with its own Silero speech gate. Select it
explicitly with `VOICETYPE_ASR_PROFILE=nano`; missing assets, failed checks or an
unsupported profile fail rather than silently loading another model.

This is an advanced, pinned integration. Native binaries and model weights are
not included. `build.rs` verifies exact header, C API and ONNX Runtime hashes;
the stock sherpa library does not implement the required completion markers.
An independent rebuild may produce a different binary hash. Audit and verify
that build before updating pins; do not disable the checks to accept arbitrary
libraries. Source provenance, the upstream patch and build requirements are in
[the integrity patch instructions](../patches/README.nano-integrity.md).

```sh
export VOICETYPE_SHERPA_NATIVE_ROOT=/absolute/path/to/verified-native-root
export VOICETYPE_NANO_TARGET_DIR=/absolute/path/to/separate-cargo-target
bash scripts/test-nano-shim.sh
bash scripts/build-nano-preview.sh
```

The native root must contain `include/sherpa-onnx/c-api/c-api.h`,
`lib/libsherpa-onnx-c-api.so` and `lib/libonnxruntime.so`. The build helper also
enables SenseVoice, so its static libraries must be prepared as in the README.
The helper builds a candidate only; it never installs or restarts the service.

Set absolute model paths when launching that candidate:

```sh
export VOICETYPE_ASR_PROFILE=nano
export VOICETYPE_NANO_MODEL_DIR=/absolute/path/to/funasr-nano-int8-model
export VOICETYPE_NANO_VAD_MODEL=/absolute/path/to/silero-vad.onnx
```

The adapter checks model assets and runs CPU inference. Long input is segmented
at bounded speech/pause boundaries while preserving coverage; segment failure
fails the whole utterance. It rejects truncated audio, unfinished decoding,
missing completion markers, malformed outputs and non-text events. No partial
text is sent to make a failed utterance appear successful.

Nano requires a larger memory budget than the default SenseVoice service unit.
Set a suitable user-service memory limit and measure the loaded process before
adopting it. Keep a working daemon backup and restore its profile configuration
together with the binary. Build success or synthetic tests do not establish
general ASR accuracy or dictation latency improvement.

## Preparing a relocatable desktop runtime (Linux)

The opt-in `relocatable-runtime` Cargo feature enables Nano and changes the
daemon's ELF RPATH to `$ORIGIN/../lib`. The usual development build keeps its
explicit native artifact path. Exact header/C-API/ORT pins remain enforced.

```sh
bash scripts/build-desktop-runtime.sh /absolute/path/to/verified-native-root /absolute/path/to/new-bundle
python3 desktop/tests/runtime_layout.py /absolute/path/to/new-bundle
```

The helper builds in a separate ignored Cargo target with two CPU build jobs,
stages `bin/voicetyped` and the pinned native pair in `lib/`, checks actual ELF
search paths and loader resolution, and refuses an existing destination. It
does not install, start a provider or modify the daily service. Nano is the only
compiled recognizer in this candidate; the owner still selects it explicitly.

This is a runtime preparation step, **not a distributable installer**. Release
work still needs a clean build/provenance pipeline, complete dependency licenses,
system dependencies including OpenCC, and Tauri resources/activation. Do not
publish local build binaries as if those steps were complete.

The actual ownership probe accepts an optional fourth argument naming the
expected library directory. It verifies the running process maps against that
exact directory before reaping the child; it sends no Start command. A release
candidate moved to a different directory containing spaces passed both the ELF
layout check and this real CPU model-loading check. That is relocation/control
evidence, not recording or latency acceptance.
