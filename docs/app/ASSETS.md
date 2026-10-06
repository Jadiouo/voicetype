# Versioned assets for app setup

Status: local staging/verification/activation/rollback is implemented. Network
download, archive extraction, setup UI and native-runtime packaging are next.
The catalogs contain metadata only; no weights, native binaries or user data are
committed. Installing assets does not start a provider or change the daily setup.

## Installation boundary

`desktop/core/src/assets.rs` accepts a reviewed `AssetManifest` and a prepared
source directory. The manifest is supplied by the application catalog; a manifest
downloaded with arbitrary payloads is not its own trust authority.

- Copy only listed regular files into a private, distinct version directory.
  Check exact byte counts and SHA-256 while streaming; reject links, ambiguous
  Windows names, traversal, duplicate paths and a wrong platform/schema.
- Sync completed files/directories and atomically replace a small activation
  record only after all files pass. Serialize installers with a stable file lock
  and detect an externally changed activation record before replacement.
- Keep running/previous versions intact. Rollback re-verifies the selected old
  version before switching. Reinstallation reuses a verified identical version;
  repair creates a new version if the existing files changed. Corrupt previous
  versions cannot be reactivated. Unknown/invalid activation records are preserved.
- Keep the asset root separate from vocabulary, learning, recordings and desktop
  preferences. Do not delete old versions while a process may still map them.
  Cleanup of unreferenced versions and download cancellation/progress are pending.

`active()` performs full integrity verification for setup/activation. It must not
be called on every recording or status refresh. No hashing/copying belongs in the
dictation path. Atomic publication is implemented on both target platforms;
directory fsync is additionally used on Unix. Power-loss behavior on a particular
Windows filesystem is not established by the current tests.

## Pinned model sources

`desktop/assets/nano-models.json` pins six files from the upstream
[GitHub download documented by sherpa](https://k2-fsa.github.io/sherpa/onnx/funasr-nano/pretrained.html).
`downloads.json` pins its compressed size (841,730,611 bytes), archive SHA-256
`eb43d7ccc2e86b243f6a03b7df361033dda66db9523d1a92bf6aca2b50c9476b`
and the single top-level archive directory. On 2026-10-07 the actual downloaded
archive matched that digest, and all six extracted-file hashes matched the
existing daily Nano model. The catalog version includes the GitHub asset's
2026-04-12 publication date rather than assuming its filename is immutable.

Do not substitute the similarly named
[Hugging Face export](https://huggingface.co/csukuangfj/sherpa-onnx-funasr-nano-int8-2025-12-30/tree/6f16bd378457e13f36ccf3910df9017f96c346fb).
At that revision its three ONNX hashes differ from the selected GitHub release;
the tokenizer files match. This is a byte-level difference, not an accuracy or
performance comparison. A future URL replacement must still satisfy the pinned
archive and extracted-file hashes.

`silero-vad.json` pins the v5.0 ONNX from upstream commit
[`5cd2ba54db059f961e7545d4f211b44f005a6dfb`](https://github.com/snakers4/silero-vad/tree/5cd2ba54db059f961e7545d4f211b44f005a6dfb).
The actual upstream download and daily copy both contain 2,313,101 bytes with
SHA-256 `6b99cbfd39246b6706f98ec13c7c50c6b299181f2474fa05cbc8046acc274396`.

The [base Nano model card](https://huggingface.co/FunAudioLLM/Fun-ASR-Nano-2512)
declares Apache-2.0. The selected archive README credits the
[ONNX export project](https://github.com/Wasser1462/FunASR-nano-onnx) and its
ModelScope files; the archive has no separate license file. The catalog's Nano
license field records the base model declaration, not a new license grant for
the export project's code. No export scripts are bundled. Silero supplies an
[MIT license](https://github.com/snakers4/silero-vad/blob/5cd2ba54db059f961e7545d4f211b44f005a6dfb/LICENSE).
Production packages/setup must carry the complete applicable license/notice
inventory; these source links alone do not finish that packaging task.

## Evidence and next integration

Five public installation tests use actual temporary files: corrupt update,
incomplete staged bundle, upgrade/rollback, preservation of invalid activation
records and reuse/repair. The latter preserves the original version and refuses
rollback into its subsequently corrupted files.

`probe_asset_install MANIFEST SOURCE_DIRECTORY` copies real reviewed model files
to a disposable store and reopens/verifies the committed version through the
public API. It passed for all six Nano files and the Silero model. The models were
not executed, and the daily files were read only. This establishes file integrity
and installation behavior, not speech, latency or clean-machine setup acceptance.

Next implement the bounded HTTPS download/extraction worker using these catalog
pins, with progress/cancellation off the resident dictation worker, then connect
explicit setup/activation in Tauri. Native CPU libraries and the engine executable
need their own platform-specific manifests/build provenance and relocatable
packaging; model-only installation is not a working recognizer.
