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
