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
