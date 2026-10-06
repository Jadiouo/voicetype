# VoiceType Desktop — Software Design Document

Status: approved product scope; implementation in stages. This document describes
the intended product, not a claim that Windows dictation already works.

## 1. Product and acceptance requirements

VoiceType becomes a tray application with an installer, rather than requiring a
user to assemble services and edit files. Both platforms offer the same choices:

| ID | Requirement |
| --- | --- |
| APP-01 | Choose Local (offline Nano) or Google (official Antigravity CLI); persist the preference across restarts. Both choices remain visible with honest setup status. |
| APP-02 | Local inference defaults to CPU. Never silently use CUDA or switch from Local to a network provider. |
| APP-03 | One active recording, bound to its original provider and input target. Busy provider changes are rejected. Cancelled, stale and duplicate results are never delivered. |
| APP-04 | Preserve traditional Chinese, English, pauses, protected names and explicit vocabulary rules. Keep the existing bounded spelling policy; no generative rewriting. |
| APP-05 | Shared vocabulary/settings and optional local review: up to 5 samples/day, 8+ seconds, 7-day expiry; user explicitly approves permanent rules. No surprise review popups. |
| APP-06 | Easy installation, repair and upgrade; preserve existing vocabulary, learning data, review choices and samples. App installation and user data have separate lifetimes. |
| APP-07 | First-run guidance accurately distinguishes missing runtime, missing models, login required, ready and error. Selecting a provider does not download, log in or record. |
| APP-08 | Linux and Windows share the application logic and UI. Platform integrations are tested on their actual OS before release. |
| APP-09 | App shell must not add a text-stability delay or model to the dictation path. Record capture-ready, stop/drain, inference, correction and delivery timings without transcript/audio in diagnostic logs. |
| APP-10 | Failure is visible. Never advertise a shell-only build as a working dictation installer, or acknowledge an unverified install as successful. |

The existing Linux implementation and PR #1 are the reference behavior. No new
recording trial is required to merge that already-tested change. New platform
code has separate acceptance gates in [TDD.md](TDD.md).

## 2. Initial support matrix

| Area | Linux first release | Windows first release |
| --- | --- | --- |
| Platform | Ubuntu 24.04 x86-64, Fcitx5, supported desktop tray extension | Windows 11 x86-64, normal desktop session |
| Installer | `.deb`, declared system dependencies, desktop entry | NSIS per-user `setup.exe`, WebView2 bootstrap if absent |
| Shell | Tauri 2, Rust commands, bundled local HTML/CSS/JS | Same |
| Local engine | Existing Nano CPU adapter and pinned native patch | Same model/policy, Windows CPU DLLs built from the reviewed native source |
| Google | Official CLI via existing interactive terminal adapter | Official CLI via ConPTY; voice and editor capture must pass real Windows validation |
| Input integration | Fcitx5 owns focus and text commit | Win32 shortcut/focus and Unicode input adapter; TSF is a later option |
| IPC | Existing private Unix socket behind a platform adapter | Inherited child stdio or per-user restricted named pipe; never an unauthenticated TCP command server |

Windows ARM, macOS, elevated/secure-desktop targets, arbitrary Linux distributions
and automatic updates are outside the initial support promise. An AppImage would
not remove Fcitx host integration requirements, so it is not the first installer.

## 3. Architecture

```text
Tray / settings / first-run setup (Tauri, bundled local UI)
                          |
                 Typed application commands
                          |
          Shared Rust application core and preferences
             |                 |                |
      Session coordinator   Vocabulary/review   Runtime setup
             |                 |                |
       Provider adapter     Shared policies   Verified artifacts
         /         \
   Local Nano     Google official CLI
         \         /
        Platform capture / process / IPC adapters
                          |
         Focus-checked delivery (Fcitx5 / Windows)
```

The UI does not process audio and is not on the time-critical path. Engine crashes
stay isolated from the shell and desktop input process. CPU spelling remains a
bounded optional worker, not a new UI dependency. A single coordinator arbitrates
the microphone; merely opening settings never starts recording.

### Decisions

1. **Tauri 2 + Rust core.** Reuse the Rust investment and keep UI state portable.
   Avoid a wholesale rewrite of validated recognition. A plain bundled frontend
   needs no network resources or remote web content at runtime.
2. **Preferences are separate from readiness.** A user may choose Google before
   logging in or Local before downloading models. This saves a preference but
   cannot start a session until that provider is ready. Never silently fall back.
3. **A separate desktop configuration file.** Linux keeps existing `voicetype/`
   user data paths; Windows uses per-user app data. `desktop.json` owns only the
   desktop schema. Old vocabulary and learning files are not rewritten on startup.
   Reject unknown newer schemas; atomically replace preferences and detect
   concurrent writes rather than clobber another running instance.
4. **Freeze a session.** Capture the provider and target at start, prohibit changes
   while recording/finalizing, invalidate cancellation before waiting for a child,
   and accept at most one final result for that session. Focus is rechecked at the
   OS delivery boundary. Keep undelivered text visible, never force focus back.
5. **Preserve correction semantics.** The shared vocabulary/review UI replaces the
   GTK front end gradually; port storage locking, atomic replacement and OpenCC
   paths explicitly. Do not run Unix `fcntl` code on Windows or silently downgrade
   traditional conversion when data is missing.
6. **Google stays official.** User installs/logs into Antigravity with its official
   flow. Voice is interactive (not `--print`); never send Enter to submit the
   dictated prompt to its agent. Keep complete PCM drain, one stop, two captures,
   errors and focus checks. PCM catchup remains an optional, separately validated
   setting. No copied credentials, unofficial API, or paid API fallback.
7. **Versioned assets.** Model/native manifests must pin URL, version, SHA-256,
   platform and license. Download to staging, verify, then activate atomically;
   interruption keeps the old working runtime. Native Windows assets require their
   own provenance; a Linux `.so` hash says nothing about a Windows DLL.
8. **No ambient migration.** Preview builds use a distinct app identifier and do
   not start/stop existing services or take their shortcuts. Production migration
   first detects active sessions, then switches the owning input integration with
   a rollback record. App uninstall retains user data by default.

## 4. User flow

First run offers two cards: **本機離線** (CPU, one-time model download) and
**Google** (internet and official account login). Each shows what is missing and a
concrete setup action. Selection is saved immediately; recording controls remain
disabled until verified ready. No simulated Ready or fake transcript.

The tray opens settings and shows the selected provider, recording state and
pending reviews. Settings include recognition, vocabulary, review and general
preferences. Changing provider when idle affects the next recording. When busy,
explain that the current recording must finish or be cancelled first.

Vocabulary offers explicit wrong/right examples such as `geeho → GitHub`, while
ambiguous words such as `coming` need a full phrase. Review corrections remain
local; promoting a short rule requires an explicit action. Daily dictation does
not prompt for approval of each automatic correction.

## 5. Implementation stages and release gates

| Stage | Deliverable | Exit evidence |
| --- | --- | --- |
| M1 | Shared preferences/provider commands, honest setup shell, Linux/Windows preview installer CI | Public command tests, actual Linux/Windows build artifacts, isolated shell launch; explicitly no dictation claim |
| M2 | Linux end-to-end app integration, shared vocabulary/review, pinned model/runtime installation | Clean-account install, provider switch, stop/cancel/focus checks, upgrade/rollback with user data preserved |
| M3 | Windows capture/shortcut/delivery, Nano CPU DLL and Google ConPTY adapters | Windows native build plus real microphone, login, traditional/mixed-English and target-app trials |
| M4 | Production packages, migration/repair/uninstall and documentation | Both providers pass the platform acceptance matrix; hashes/license inventory and upgrade tests; no fabricated Windows evidence |

Milestones are implementation order, not permission checkpoints. Work proceeds
after the requested TDD seam agreement. Existing production services stay usable
throughout. Preview installers are development artifacts, not a stable release.

## 6. Risks and evidence

- Current Rust daemon IPC, Python terminal/relay, GTK GUI and some file locks are
  Unix-specific. CPAL portability alone does not make the whole app portable.
- Windows input injection cannot cross privilege boundaries reliably; UIPI permits
  only equal/lower integrity targets. Successful `SendInput` counts must be checked,
  and a partial insertion must not trigger an automatic full-text retry. See
  [Microsoft SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput).
- Google documents Windows installation and microphone permissions, but that is
  not proof that our ConPTY/editor-capture integration works. Verify separately:
  [CLI install](https://antigravity.google/docs/cli/install/) and
  [voice](https://antigravity.google/docs/cli/commands/voice/).
- Tauri supports NSIS and MSI packaging. Build on Windows CI first; real desktop
  behavior still needs an interactive Windows session. See
  [Windows installers](https://v2.tauri.app/distribute/windows-installer/) and
  [prerequisites](https://v2.tauri.app/start/prerequisites/).
- Runtime/model redistribution licenses and asset hashes must be checked before
  bundling. Repository root is GPL-3.0; existing crate metadata has inconsistent
  MIT labels. New desktop code follows the repository license; packaging must not
  imply that third-party model or CLI licenses are covered by that choice.
- No official daily/monthly voice-minute allowance has been established. Do not
  label generic model refresh/weekly limits as a voice quota.

## 7. Progress and continuation

See [DEVLOG.md](DEVLOG.md) for completed work, measured evidence and the next
vertical slice. The working branch is `feat/desktop-app`, based on main after
merge commit `8c67ce4` (PR #1). Private recordings, personal dictionaries, credentials
and private development history are excluded from public commits.
