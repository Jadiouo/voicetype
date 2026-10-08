# Versioned assets for app setup

Status: verified storage, bounded HTTPS/archive preparation and explicit
download/check/cancel UI, Linux CPU runtime and explicit Fcitx setup are implemented.
Windows native-runtime packaging and activation remain. An installed model is not
a connected recognizer.
The catalogs contain metadata only; no weights, native binaries or user data are
committed. Installing assets does not start a provider or change the daily setup.

Linux `build-desktop-input.py` builds the repo's Fcitx module with required native
Caps Lock support and produces an exact two-member module/license catalog. The
Tauri build embeds this catalog and packages its bytes; the GUI cannot supply
replacement manifests or module paths. Explicit installation reuses AssetStore,
then writes a rollback journal before replacing only the per-user addon
registration. Existing module binaries/settings are preserved. Interrupted
publication can be retried; subsequent external registration changes block both
update and restore. No Fcitx restart, microphone access or shortcut takeover is
part of installation. Keep the original installation until live acceptance.

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
  Copy and verification report progress/check cancellation every 64 KiB and
  before publication. Cleanup of old/unreferenced versions remains pending.

`active()` performs full integrity verification for setup/activation. It must not
be called on every recording or status refresh. No hashing/copying belongs in the
dictation path. Atomic publication is implemented on both target platforms;
directory fsync is additionally used on Unix. Power-loss behavior on a particular
Windows filesystem is not established by the current tests.

## Download and setup commands

`ModelSetup` owns a separate thread from the resident dictation worker. Opening
the app does not start setup. The webview can only start, read status or cancel;
URLs, manifests and file paths come from the compiled catalog. Full verification
of an identical installed catalog version permits reuse without network access.

- HTTPS uses normal platform certificate verification, at most five HTTPS
  redirects, 15-second connect, 20-second stalled-read and 30-minute total request
  bounds. It sends no cookies or account credentials, uses no automatic retries,
  environment proxy or content decoding, and checks exact bytes/SHA while reading.
  See the upstream [reqwest builder](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html).
- Cancellation can drop a pending async request, including a stalled TLS
  handshake. Setup shutdown joins its thread; a bounded Tokio shutdown prevents
  a blocking OS DNS lookup from holding cancellation indefinitely. Disk calls
  are synchronous on the setup thread, so this is not a hard real-time deadline
  for a stalled filesystem.
- Recheck the complete compressed payload before extraction. The tar reader
  accepts ordinary regular files/directories only, rejects extended headers,
  links, traversal and duplicate paths, and enforces a 10,000-entry limit plus a
  decompressed-byte limit (listed files plus 64 MiB for upstream extras). Only
  pinned files are written. Unlisted documentation/test audio is discarded.
- Progress names download, checking, extraction and installation separately.
  Extraction shows processed bytes without an invented completion percentage.
  Completed model bundles are retained if a later bundle is cancelled; partial
  scratch/staged data is removed on normal cancellation/error. A process crash
  can leave unreferenced temporary files; crash cleanup/resume remains pending.

The active version is published only after verification. Cancellation arriving
after that commit point cannot undo a successful installation. Installed model
status does not load the engine, activate a hotkey or migrate the daily setup.

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

Nine public installation tests use actual temporary files: corrupt update,
incomplete staged bundle, upgrade/rollback, preservation of invalid activation
records and reuse/repair. The latter preserves the original version and refuses
rollback into its subsequently corrupted files. Additional cases exercise
mid-file copy/extraction cancellation and corrupt/unsafe archives. Three setup
cases cover a real stalled TLS peer, offline reuse, and cancel/retry while the
desktop settings worker continues accepting commands.

`probe_asset_install MANIFEST SOURCE_DIRECTORY` copies real reviewed model files
to a disposable store and reopens/verifies the committed version through the
public API. It passed for all six Nano files and the Silero model. The models were
not executed, and the daily files were read only. This establishes file integrity
and installation behavior, not speech, latency or clean-machine setup acceptance.

`probe_model_setup nano-cache ARCHIVE` exercised the actual pinned Nano archive
through the production decoder and installer, including all six file hashes.
`probe_model_setup vad-download` exercised a real upstream HTTPS download and
installation. Both reopened and matched the catalog in disposable stores without
executing any model. The installed GUI/network combination still needs separate
acceptance; the CI shell test only verifies explicit-only setup and initial UI.

`probe_model_setup all-download` also completed the entire compiled-catalog
worker flow: actual GitHub Nano redirect/download plus VAD, installation and
reopening both bundles. The real network source and installer were used together,
without model execution. This remains separate from interactive GUI acceptance.

## Linux packaged runtime

`linux-native-sources.json` fixes the source/dependency archives, ORT headers,
official CPU shared library and Microsoft license/third-party notices by URL,
size and SHA-256. `build-desktop-native.py` verifies inputs before extracting
ordinary files/directories, applies the pinned integrity patch, checks patched
file hashes, and builds with CMake FetchContent disconnected and every optional
Sherpa capability disabled except C API. No installed private library is an input.
The result includes source-build provenance, patch and native dependency notices.

`build-desktop-runtime.sh --source-built` verifies that build output and compiles
the Nano daemon against its exact C API digest. Different compilers need different
output digests; the fixed ORT/header pins and the legacy development C API pin
remain unchanged. A release-builder digest is accepted only for the isolated
relocatable feature. It is a build-time input, never an installed sidecar override.
Diagnostics remap builder source paths; both native libraries resolve relative
to the bundled executable. Ubuntu runtime dependencies include OpenCC data/library,
ALSA and the C++ standard library.

The Linux Tauri build embeds the generated catalog after checking every payload
hash and includes the runtime resource directory in the `.deb`. On explicit load,
`LocalInstaller` first checks installed models against the compiled model catalog,
then installs runtime bytes into a private immutable version using the embedded
catalog. Replacing a resource manifest cannot authorize different executable bytes.
Opening settings does no runtime installation or process launch. Hashing/copying
runs off the resident dictation worker; warm recording never rehashes assets.

CI rebuilds the native dependencies, checks extracted installer bytes against the
build-time catalog and verifies real loader paths. Windows still has no packaged
speech runtime. Shared input integration, complete app/model/Rust dependency notice
inventory and real speech acceptance remain release work; this preview is not a
production dictation installer.


### Windows vocabulary conversion

`scripts/build-desktop-opencc.py` builds OpenCC 1.1.9 from commit
`556ed22496d650bd0b13b6c163be9814637970ae`, with source archive SHA-256
`2792fc0944359c5d099bd79e08bfffce250bf3760aa0ec306e975cc58314482a`.
The recipe disables Darts, benchmarks and tests, uses static MSVC runtime and
marisa, and packages only the DLL plus `s2tw.json`, STPhrases, STCharacters and
TWVariants. Each produced file is cataloged and verified by the Tauri build.
OpenCC's Apache-2.0 notice, marisa's BSD-2-Clause choice and the pinned RapidJSON
notice are included; provenance records their sources. The DLL is loaded only
from beside the application executable, with its absolute dictionary path.
See [upstream build options](https://github.com/BYVoid/OpenCC/blob/556ed22496d650bd0b13b6c163be9814637970ae/CMakeLists.txt).
This supplies text conversion, not the still-incomplete Windows speech runtime.

### App-owned CPU spelling bundle

`scripts/build-desktop-spelling.py` runs in an isolated Python 3.12 environment
from `config/csc-build-requirements.txt`. It verifies the author's MacBERT ONNX
revision `615e6e09ef9a69ec487bc7c641ec3a311e2c11b9`, its source model and
tokenizer SHA-256 values in `scripts/prepare_csc.py`, and produces a CPU-only
INT8 ONNX model. PyInstaller freezes the private stdio worker separately on Linux
and Windows. The build catalog pins each resulting file's bytes and SHA-256; the
Tauri build embeds the catalog and checks the resource bytes. Explicit engine load
installs the reviewed package through `SpellingInstaller`, then the daemon owns a
single child through private pipes. It accepts positional Chinese-character edits
only within the original 100 ms correction budget, retaining the input on timeout,
crash, malformed response or protected text. No GPU inference or microphone is
part of the packaging/model probe.

The Linux bundle's `provenance/binary-origins.json` inventories every native
extension, PyInstaller bootloader and copied shared library. The builder matches
system library bytes to the builder's installed Debian packages, records their
versions and includes package copyright texts, including the GCC runtime library
exception. NumPy's wheel license contains its OpenBLAS, libgfortran and libquadmath
terms. Python distribution notices, the MacBERT Apache-2.0 notice and model card
are included. Unknown Linux native binaries fail the build. The local candidate
has 207 catalog members and 59 accounted native binaries.

Windows builds emit the exact native DLL/PYD inventory with `complete=false`.
MSVC runtime origin, redistribution permission and notices must be reviewed from
the actual Windows CI artifact before a release claim. Complete application Rust
dependency notices and Windows speech acceptance also remain open release work;
the preview CI artifact is not a production release.
