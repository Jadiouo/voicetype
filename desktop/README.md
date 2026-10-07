# VoiceType desktop development preview

This branch starts the Linux/Windows application described in
[SDD](../docs/app/SDD.md) and [TDD](../docs/app/TDD.md).

**Linux now has an explicit Fcitx-to-app local dictation path. Google and Windows
speech integration, shared vocabulary/review/CSC and live acceptance remain
unfinished.** Keep the existing installation available during preview testing.

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
app run. This currently uses an isolated app data profile, so existing vocabulary
and spelling settings are not yet shared with app dictation.

"Check services" sends a read-only ping to an existing Linux local engine and
distinguishes response, missing service, timeout and incompatible reply. It does
not start audio or establish Google login readiness. The shared core now also
tests provider/target ownership, cancel, late/duplicate results and failed input
delivery. Native Fcitx/worker integration runs against a fixture capture/model;
these checks do not establish live recognition quality or end-user latency.

The preview has its own application ID and settings directory:
`io.github.jadiouo.voicetype.preview`. It never imports or changes the existing
`voicetype` vocabulary/learning settings. Only the explicit module installer changes
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
