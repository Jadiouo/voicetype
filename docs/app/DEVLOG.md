# Desktop app development log

## 2026-10-07 — main merged; app design prepared

- PR #1 merged into GitHub main as `8c67ce4`; its reviewed head remained `c3aaec0`.
  Reused the completed validation evidence instead of rerunning the same suite.
- Created clean `feat/desktop-app` worktree from merged main. The unrelated local
  main edits and running dictation installation are preserved.
- User confirmed the two selectable providers are Local offline recognition and
  Google official CLI. Vocabulary and sampled review are common features.
- Wrote SDD and TDD before new implementation. Tauri/Rust, Linux `.deb`, Windows
  per-user NSIS installer; CPU default, no automatic provider fallback.
- Existing Unix IPC/PTY/PipeWire/GTK/locking requires explicit platform adapters.
  Official Google documentation establishes Windows availability, not acceptance
  of our planned adapter. Windows runtime verification remains outstanding.
- Local build environment lacks GTK/WebKit development packages and passwordless
  package installation. Prefer native Linux/Windows CI builds to changing the
  user's workstation. No models downloaded, microphones opened or services changed.
- Next: agree the four public test boundaries in TDD, then implement the first
  provider/preference command slice with red-before-green evidence.

## 2026-10-07 — M1 preference commands and shell

- User confirmed all four TDD boundaries. New Rust workspace is independent of the
  Linux-only daemon so public command tests run without audio/GTK dependencies.
- Five vertical red → green cycles completed: choose/reopen (initial unresolved
  command interface); reject newer schema (was accepted); reject stale-window
  overwrite (was accepted); preserve unknown settings (was dropped); reload after
  conflict (missing command). All five now pass through public application APIs.
- Writes use a stable per-file OS lock, compare the last-read bytes, and atomically
  persist a same-directory temporary file. Unknown same-schema fields survive.
  Lock-aware writers serialize; arbitrary external editors can still ignore the
  lock, so this is not a universal filesystem transaction guarantee.
- Added a bundled Traditional Chinese Tauri settings window, Local/Google choice,
  explicit unavailable adapter state, error/reload feedback, tray entry and single
  instance handling. Preview uses a separate app ID/data directory and does not
  alter the daily dictation installation. Normal close exits the preview.
- Added locked Rust/npm dependencies and Linux/Windows native packaging workflow.
  Core clippy passed; native installers and UI interaction still await CI evidence.
  No claim of complete packaged dictation or a verified Windows microphone path.

## 2026-10-07 — first Linux/Windows packages built

- CI run `37498271515`, source `965fd80`, succeeded on Ubuntu 24.04 and Windows
  Server 2022 runners. Five command tests passed independently on both operating
  systems. Linux `.deb` was extracted and its desktop entry checked; Windows NSIS
  installer ran silently in the disposable runner and the installed `.exe` existed.
- Downloaded the Linux artifact and verified its 4,261,590-byte package against the
  CI SHA-256 manifest. Payload desktop command is `voicetype-desktop`; the local
  runtime dependency check reported no missing libraries. This is package evidence,
  not a native-window or speech test.
- Added a real WebDriver harness for the installed shell: Local default, Google
  selection, restart persistence, corrupt-settings error and repair/reload. Uses an
  explicit disposable preview config directory (Windows app-data APIs do not
  reliably honor a modified APPDATA environment). No fake Tauri commands.
- Next CI revision adds actual Linux package installation, platform UI drivers,
  screenshots, bundled license and preview/source documentation. UI verification
  is pending until that workflow runs successfully. Windows 11 microphone/target
  app acceptance remains separate from a Windows Server CI shell test.
