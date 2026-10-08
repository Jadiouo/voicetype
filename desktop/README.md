# VoiceType desktop development preview

This branch starts the Linux/Windows application described in
[SDD](../docs/app/SDD.md) and [TDD](../docs/app/TDD.md).

**Linux now has an explicit Fcitx-to-app local dictation path and an optional
app-owned CPU spelling worker. Google and Windows speech integration and live
acceptance remain unfinished.** Keep the existing installation available during preview testing.

Model download/check/cancel is available in the UI. Linux installers now carry a
source-built Nano CPU runtime: "載入本機引擎" verifies installed models and packaged
bytes, then loads a private owned engine; "停用並卸載引擎" releases it. Loading does
not record or acquire input. Windows runtime loading remains
unavailable, with an explicit message in the same UI.

On Linux, choose **安裝／更新 Fcitx 模組**, then log out/in when convenient to load
it. This installs verified versioned files and backs up the exact prior per-user
addon registration; it does not restart Fcitx or change daemon services. The
**還原原模組設定** action restores that registration, including its prior absence.
External edits are preserved and reported as conflicts. Existing personal
vocabulary, Fcitx preferences and module binaries are not replaced.

After preparing models/loading the runtime, choose **啟用本機聽寫**. Wait for the
connected status, focus an ordinary text field, hold **Ctrl+Alt**, speak and release.
**Esc** cancels. The same module switches only after the previous dictation's
pending result ends; no second hotkey owner is created. Closing/unloading the app
withdraws the private lease and returns to the original service. A crashed or
unresponsive app also falls back; a clean account without a legacy daemon simply
has no dictation until the app is enabled again. Input setup is explicit on each
app run. App vocabulary and sampled review use their shared settings described
below. The app-owned spelling worker is separate from the legacy service.

"Check services" sends a read-only ping to an existing Linux local engine and
distinguishes response, missing service, timeout and incompatible reply. It does
not start audio or establish Google login readiness. The shared core now also
tests provider/target ownership, cancel, late/duplicate results and failed input
delivery. Native Fcitx/worker integration runs against a fixture capture/model;
these checks do not establish live recognition quality or end-user latency.

The preview has its own application ID and settings directory:
`io.github.jadiouo.voicetype.preview`. It never changes the existing
`voicetype` vocabulary/learning settings; vocabulary import requires an explicit action. Only the explicit module installer changes
the Fcitx addon registration, with rollback. Closing its window exits the preview; a desktop tray entry
also opens settings where the desktop supports it. No autostart is installed.

## Development

Use Rust 1.90+ and Node 22+. For native prerequisites, follow
[Tauri's platform instructions](https://v2.tauri.app/start/prerequisites/).

Before a Linux Tauri build or `dev`, prepare the native runtime from the repo root
(Ubuntu 24.04; requires CMake, Ninja, C/C++ toolchain, curl, patch, `libasound2-dev`
and `libopencc-dev`, in addition to Tauri prerequisites):

```sh
python3 scripts/build-desktop-native.py --cache private/native-source-cache --output private/release-native
bash scripts/build-desktop-runtime.sh private/release-native desktop/target/runtime --source-built
python3 scripts/build-desktop-input.py desktop/target/input
```

For optional CPU spelling, use an isolated Python 3.12 environment with
`config/csc-build-requirements.txt`, then build a fresh bundle on each target OS:

```sh
python scripts/build-desktop-spelling.py desktop/target/spelling --cache private/csc-release-cache
```

The builder checks the author's fixed model revision and source hashes, prepares
an INT8 CPU model, freezes the private pipe worker and records every packaged
file hash. The installed app verifies that catalog before loading the local
engine. The **自動修正中文錯字（CPU）** setting persists across restarts; unload the
engine before changing it. A failed engine can be explicitly unloaded before
retrying. Correction is bounded to 100 ms and keeps the original text on timeout,
invalid edits or worker failure. Linux native dependency notices are inventoried;
Windows MSVC/runtime notices still require review of its actual CI bundle. See
[asset provenance](../docs/app/ASSETS.md).

Before packaging, catalog the locked Rust dependencies actually reachable from
the target App (and Linux's bundled daemon), together with the native catalogs.
The script copies each crate's license/notice text into the installer and fails
if an included crate has no verified text:

```sh
# Linux, after runtime/input/spelling preparation
python scripts/build-desktop-rust-notices.py desktop/target/rust-notices \
  --target x86_64-unknown-linux-gnu \
  --component runtime=desktop/target/runtime --component input=desktop/target/input \
  --component spelling=desktop/target/spelling
# Windows, after OpenCC/spelling preparation
python scripts/build-desktop-rust-notices.py desktop/target/rust-notices \
  --target x86_64-pc-windows-msvc \
  --component opencc=desktop/target/opencc --component spelling=desktop/target/spelling
```

Use only the command for the platform being packaged. The output directory must
be fresh; installed tests compare every notice byte and native catalog hash.

The module build additionally needs `extra-cmake-modules`, `libfcitx5core-dev`,
`libfcitx5utils-dev`, `libfcitx5config-dev`, `fcitx5-modules-dev`, `libxcb-xkb-dev`
and `libxcb-ewmh-dev`. Release builds refuse a module missing X11 Caps restoration.

All outputs must be new directories. Retain or move previous candidates before
rebuilding. Downloads are hash-pinned; `--offline` uses only a complete verified
source cache. This is CPU compilation and does not download speech models. See
[asset provenance](../docs/app/ASSETS.md) for the build/runtime trust boundary.

```sh
cd desktop
cargo test -p voicetype-app-core --locked
npm ci --ignore-scripts
npm run tauri -- dev
```

Build on the target OS:

```sh
# Linux
npm run tauri -- build --ci --bundles deb -- --locked
# Windows
npm run tauri -- build --ci --bundles nsis -- --locked
```

Artifacts are under `desktop/target/release/bundle/`. GitHub Actions builds both
platforms and uploads development artifacts with their source commit and hashes;
it does not publish a production release. Windows previews are unsigned and their
installer may show the normal Windows publisher warning. No certificate is
silently obtained and no signing secret is stored in the repository.

The static UI is bundled locally, has no remote scripts, and cannot directly run
shell commands or access arbitrary files. Only typed Rust commands expose settings.
Core tests use temporary directories; no microphone, live dictation, GPU, account
login or text injection is part of preview validation.

The CI UI check runs the installer payload with a real platform WebDriver, chooses
Google, reopens the app, and checks error/reload behavior. To use a disposable
profile, set `VOICETYPE_PREVIEW_CONFIG_DIR` to an absolute temporary directory.
This overrides only the preview's configuration directory. Isolate `XDG_DATA_HOME`
as well when testing module installation; the setting is never
used by the production dictation daemon. UI evidence is saved separately from
installer artifacts. Neither is proof of working speech adapters.


### Vocabulary in the preview

Open **我的詞庫** to add explicit wrong/right spellings, protect names, preview
saved rules, or restore the previous save. The preview stores `vocab.toml` beside
`desktop.json`; both installed providers will use that file, with Local already
connected. Rules are reloaded at each local output without restarting the engine.
Preview checks vocabulary replacements only, not ASR, conversion or CSC.

**匯入原有詞庫** explicitly copies the existing `voicetype/vocab.toml` (or the
native `VOICETYPE_VOCAB` override) only before an App vocabulary has been created.
The original remains unchanged. Conflicting edits request reload instead of
silently overwriting another editor; saves keep one `.bak` version. TOML comments
and unknown fields are preserved. Use a complete phrase for ambiguous English
such as `coming`; never assume every occurrence means `commit`.

Windows builds first run `python ../scripts/build-desktop-opencc.py target/opencc --stage-tests target/debug/deps` from `desktop/`. This builds the pinned native
converter and dictionaries, stages the actual Rust test dependency, and supplies
installer resources. It does not install or start an input service. Linux uses
its declared system OpenCC dependency. Missing native conversion is an error,
not permission to discard protected names.

### Sampled review

Open **抽樣校對** and enable sampling explicitly. App-owned local dictation uses
`review.json` and `review/` beside `desktop.json`, separate from the daily install.
Default: off, at most five samples per local day, only 8–61 second recordings,
seven-day retention. Sampling never opens the microphone independently; the
existing collector queues a selected sample only after queuing its final result.
Disabling is rechecked before disk writes. The tray shows the pending count and
opens this page; it never opens a review popup on its own.

Select a sample, load its WAV, then play it with the audio controls. Confirm or
correct the full sentence and save. **加入詞庫** is a separate explicit action:
only a conservative single short replacement is offered, ambiguous words keep
context, and rules affecting another confirmed use are rejected. Failed promotion
keeps a retryable intent; repeated retries do not duplicate rules. Concurrent
changes require reload and preserve unsaved edits. Delete removes both text and
audio. Expiry also cleans damaged/abandoned recordings; the app must be running
(or subsequently reopened) for cleanup to execute.

Google and Windows speech collection remains unavailable until those adapters
are connected. The shared review commands/editor work on both platforms; fixture
WAV tests are not evidence of live microphone or recognition quality.
