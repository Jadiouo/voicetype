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

## 2026-10-07 — M1 installed settings preview verified

- [CI run 37499241303](https://github.com/Jadiouo/voicetype/actions/runs/37499241303)
  succeeded for source `9abc9a6` on both platforms. Linux installed the `.deb` using
  dpkg and launched `/usr/bin/voicetype-desktop`; Windows installed the NSIS `.exe`
  and launched its installed payload. Each independently passed all five public
  command tests and the six UI checks recorded by `shell_smoke.py`.
- UI evidence: real installed window; Local initially selected; selecting Google;
  restarting preserves Google; corrupt config produces a visible error and disables
  writes; restoring the file and reloading recovers. Linux WebKit and Windows
  WebView2 screenshots were downloaded and visually checked: Traditional Chinese
  labels render, Google is selected, and the unavailable-adapter notice is visible.
  These checks do not verify tray visibility or dictation in other target apps.
- Both downloadable artifacts were retrieved and matched against their CI source,
  size and SHA-256 manifest. Linux license and preview guide contents also matched
  the repository. The Windows executable had already run through native CI UI;
  inspecting a PE header alone was not counted as functional verification.

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| Linux amd64 `.deb` | 4,276,970 | `0ffc1156c96b323aefd732f1830c2864bc6286662ddd2dba0ab1bbdbef23352d` |
| Windows x64 NSIS `.exe` | 2,085,931 | `d880a25090bcbc6ff840115102db6d0b5e19f8abf4e28f6507251e0b56554457` |

- Public branch `feat/desktop-app`, [draft PR #2](https://github.com/Jadiouo/voicetype/pull/2).
  Remote source tree at `9abc9a6` matched all 161 local blob hashes. Subsequent
  documentation updates do not change the tested desktop source.
- No production service, hotkey, user dictionary, learning file or review data was
  changed. No models were downloaded and no live recording was made. Preview
  artifacts are unsigned development builds, not a production dictation release.

### Next implementation slice (M2)

Continue through the four already-agreed seams; do not ask for that agreement again
or rerun PR #1's full suite. First implement real provider readiness through the
application command boundary and the existing Linux IPC adapters. Selecting a
preference must ultimately route the next recording through one coordinator;
changing only this JSON file does not change the existing Fcitx hotkeys/services.

Then connect session cancellation/focus/deduplication, shared vocabulary and review,
and versioned model/runtime setup. Preserve the current Linux data formats and
the working daily installation until migration has an actual rollback path.
Windows requires the Nano native CPU build plus platform capture, focus/delivery
and Google ConPTY work. A Windows Server CI settings pass is not Windows 11 real
microphone/login/target-app acceptance. M2–M4 remain open in SDD.

## 2026-10-07 — full-app goal activated; M2 in progress

- User explicitly requested continued work to the full application, then asked to
  create a goal. The active goal is recorded in [GOAL.md](GOAL.md); no token budget
  was requested. Continue until the full acceptance criteria are satisfied.
- Inspected existing Linux services read-only. Local IPC has a `ping`/`pong`
  readiness request; the Google v1 bridge only accepts start/stop/cancel. Socket
  reachability alone must not become an authenticated/ready Google status.
- Started a public application-command readiness slice: a real temporary Unix
  socket answers the actual local engine ping. Test first failed because the
  command/status did not exist, then passed after implementation. Probe has a
  bounded connect/read deadline and bounded reply. It does not request recording.
- This first M2 slice is still in progress: negative cases, actual installed
  read-only probe, UI wiring and a proper Google status/control adapter remain.
  No new runtime has been deployed and no microphone has been opened.

## 2026-10-07 — M2 read-only engine status

- Added the settings "check services" command. Linux sends the existing bounded
  `ping` request; all settings IO/probes run off the webview event thread. A pong
  is labelled "service responded", not proof that capture, delivery or Google
  login works. Windows and Google remain explicitly unconnected.
- Public command tests: live pong, deadline, incompatible response, missing socket
  and oversized response. The deadline and incompatible-status cases failed for
  missing behavior first, then passed. Missing/oversized checks additionally
  verified guards already introduced with the bounded probe.
- The diagnostic example queried the installed local engine and received
  `service_available`. It used a disposable application profile and sent only
  ping; no production data, service configuration or microphone was changed.
- Core tests: 9 passed on Linux. Added native installed-UI checks for a missing
  provider and an OS-socket pong fixture; their CI result is still pending.
- Next: session ownership, busy-switch rejection and cancellation/result delivery
  through the agreed session/provider boundary. Full-app goal remains active.
