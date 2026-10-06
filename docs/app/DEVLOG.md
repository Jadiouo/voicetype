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

## 2026-10-07 — M2 provider status verified; session coordinator added

- [CI run 37510781789](https://github.com/Jadiouo/voicetype/actions/runs/37510781789)
  passed on Linux and Windows for source `f229d5b`. Both installed packages ran
  their native WebDriver checks. The new Linux check distinguishes missing service
  and pong over a disposable OS socket; it asserts the only request was ping.
  Windows and Google remain visibly unconnected. Downloaded UI screenshots were
  inspected; both display Traditional Chinese and the preview limitation.
- Added a shared application/session boundary. Starting fixes both provider and
  editable-context generation. Busy switching/reloading is rejected; repeated
  stop does not send another command. Stop before capture-ready cancels preparation.
  Cancellation invalidates results before IO and reserves the provider until an
  explicit quiescent release event. Old sessions and duplicate results are ignored.
- Delivery checks the original input-context lease at the OS boundary. Focus
  rejection or partial insertion keeps the last undelivered text for recovery and
  never automatically retries. Settings reload preserves this in-memory text.
  Status snapshots contain failure codes, never transcripts. Empty, oversized or
  NUL-containing final text is rejected before input delivery.
- Eight public session scenarios were developed sequentially red → green. Two
  runtime failures specifically exposed dropped early-stop and lost retained text
  on reload; both were corrected. All 17 Linux core tests pass, and Clippy with
  warnings denied passes. These session cases substitute external provider/OS
  boundaries; they are not evidence of a real recording or focus-safe Windows input.
- Native adapters still need explicit recording/release acknowledgements. Legacy
  local IPC does not emit its reserved state events and Google v1 has no health
  command. Do not infer cleanup merely from a final result, a write succeeding or
  a socket closing. Do not connect this coordinator to daily hotkeys until those
  ownership contracts and rollback are verified. No daily service was changed.
- Next: connect the coordinator to actual provider transports and input-context
  delivery, then vocabulary/review and runtime installation. M2–M4 remain open.

## 2026-10-07 — local engine lifecycle IPC

- [CI 37511989320](https://github.com/Jadiouo/voicetype/actions/runs/37511989320)
  passed both native desktop builds, installed UI and core tests at `88129d4`.
  The remote source tree matched all 167 local blobs.
- Local daemon Start now accepts opt-in `session_events: true`. It reports
  Recording after the capture boundary accepts the session, then Idle after
  native inference and text processing finish. A lifecycle guard follows the
  asynchronous work so cancellation or a stale result cannot release early.
  Legacy Start without the flag retains its previous result stream.
- Added a real Unix-socket daemon contract test through SessionManager/Server.
  Only OS capture and the native ASR boundary are substituted; OpenCC and the
  production text pipeline run normally. The first test failed for the missing
  capture boundary, then passed with the lifecycle implementation. It verifies
  complete Traditional Chinese/mixed-English output, event ordering and legacy
  compatibility. A follow-up regression gates native inference, cancels it, checks
  no premature Idle/result, then releases it and receives only Idle.
- All 21 affected Linux protocol, IPC, capture and new desktop-contract tests pass.
  `cargo check --features sherpa-nano --locked` also passes with the existing
  pinned CPU native dependencies. Existing unused-code warnings remain; no live
  recording, inference, service restart or user-data migration was performed.
- Added isolated Ubuntu CI for these daemon contracts; its first run is pending.
  This daemon change has not been installed in the daily service.
- Important remaining contract: Idle ends a session, not the warm microphone's
  30-second idle stream. Add an explicit suspend/close acknowledgement and versioned
  capability query before a desktop adapter can hand the microphone to Google.
  The actual app transport/input integration and M2–M4 acceptance remain open.

## 2026-10-07 — explicit local microphone handoff and native CPU probe

- [Engine CI 37513330768](https://github.com/Jadiouo/voicetype/actions/runs/37513330768)
  passed the first lifecycle contract at `690a320`.
- Added correlated `desktop_status` protocol negotiation and `desktop_suspend`.
  Outstanding preparation/recording/inference refuses suspend; after work ends,
  the audio thread closes the warm backend and ring before acknowledging. The
  normal same-provider warm path stays intact. Wire semantics and ownership
  prerequisites are documented in [LOCAL_PROTOCOL.md](LOCAL_PROTOCOL.md).
- The handoff test first failed for the missing capture operation. The capability
  query independently failed with a real IPC timeout, then passed. Regression
  checks cover pending cancelled inference refusing handoff, real audio-command
  acknowledgement, active-capture refusal, repeated suspend and reopening capture.
  All 23 affected daemon contract/protocol/IPC/capture tests pass locally.
- Built the real Nano feature against the pinned CPU native libraries and launched
  that candidate with existing read-only models and a disposable HOME/XDG profile.
  Status, suspend and ping all returned their exact expected replies. Process maps
  contained sherpa/ONNX Runtime with no CUDA/cuDNN/TensorRT libraries; startup
  reported `funasr-nano:int8:cpu` and `provider=cpu`. No Start was sent, no capture
  opened and Nano's silence warmup remains disabled. SIGINT exited cleanly and
  removed the candidate socket. This is runtime/control evidence, not a speech
  accuracy or latency measurement. The reusable probe is in `desktop/tests/`.
- Existing local, Google and spelling services remained active/running. No service
  restart, hotkey change, personal-data migration or GPU operation occurred.
- Next priority is the actual app provider transport and Fcitx delivery path,
  using the versioned contract and explicit runtime ownership. Google status,
  shared data UI, packaging assets and Windows work remain; Goal stays active.
- At source `bd6f3e8`, [engine CI 37514581633](https://github.com/Jadiouo/voicetype/actions/runs/37514581633)
  and [both desktop jobs 37514581670](https://github.com/Jadiouo/voicetype/actions/runs/37514581670)
  passed. The remote tree matched all 171 local source blobs. Desktop speech
  adapters are still unconnected; these green checks do not complete M2 or M3.


## 2026-10-07 — App/local transport and Fcitx delivery acknowledgement

- Added an app-side Linux local-engine connection. It authenticates the actual
  Unix peer UID/PID before sending commands, negotiates protocol capabilities,
  preserves partial frames across polling timeouts and retains ownership after
  uncertain writes/disconnects. Engine errors become non-transcript app failures;
  only an explicit idle event releases the session. Suspend still requires an
  exact correlated microphone-close acknowledgement.
- TDD: the actual app → Unix IPC → local daemon → OpenCC → delivery scenario
  failed first because the transport module was absent, then passed with complete
  `請檢查 GitHub。` delivered once. A capture-failure scenario next failed on an
  unhandled engine error, then passed with failure reported, no insertion and
  release only after idle. Only OS capture/native ASR/delivery are fixtures.
- Fcitx now negotiates `voicetype.fcitx.v1` and replies to app delivery requests
  after validating the original input context. The first real Fcitx harness test
  timed out waiting for negotiation (red), then passed with exactly one commit
  and `committed`, `stale` replies to duplicate requests (green). This confirms
  Fcitx called its validated-context commit operation, not that an external app
  visibly accepted text. Negative cases and transport hardening are underway.
- No candidate was installed and no live microphone/input was used. The isolated
  test build lacks optional XCB development headers; its delivery tests do not
  establish Caps Lock restoration. Daily services and hotkeys remain untouched.
- Still outstanding: native dispatcher/runtime ownership, app wiring, shared data
  UI, Google and Windows adapters, runtime installation and platform acceptance.

- Hardening found a fragmented oversized-frame bypass. A real Unix-peer regression
  failed on acceptance of that frame, then passed after enforcing the byte bound
  before parsing the terminating newline. The connection faults without releasing
  app ownership. Core tests: 18 passed; real daemon/app contracts: 5 passed.
- All 32 existing delivery cases plus 5 new app acknowledgement/focus/context/lost
  target cases pass. A lost app target never invokes legacy clipboard fallback.
  CI now also builds/runs this isolated Fcitx harness; native contract CI watches
  `desktop/core/**` because the daemon tests exercise that actual app dependency.
- This checkpoint supplies real transport and input-context acknowledgement, not
  an operational desktop dictation route. Next connect the native dispatcher,
  original frontend/provider session mapping, complete correction context and
  owned child cleanup. Do not point it at an independently controlled daily daemon.

- At source `161e003`, [engine/Fcitx CI 37518235137](https://github.com/Jadiouo/voicetype/actions/runs/37518235137)
  and [desktop CI 37518235144](https://github.com/Jadiouo/voicetype/actions/runs/37518235144)
  both passed. Downloaded Linux/Windows installers match their SHA-256 manifests
  and source commit; both installed UI reports passed seven checks and explicitly
  did not test speech adapters. All 173 remote source blobs match the local tree.


## 2026-10-07 — Native Linux dispatcher and owned CPU process

- `LocalDispatcher` now negotiates a same-user Fcitx connection, maps frontend
  sessions to engine sessions, routes Start/Stop/Cancel and acknowledges real
  local-engine lifecycle events. Final delivery waits for the original session
  and context's Fcitx reply; an uncertain acknowledgement retains text and is
  never retried. Transport failure invalidates results but does not invent release.
- TDD: the frontend → actual dispatcher/app → actual daemon/OpenCC → acknowledged
  frontend round trip first failed for the absent dispatcher, then passed. Only
  OS capture/native ASR/input frontend are fixtures; both Unix transports and
  the coordinator/text pipeline are real.
- The next case exposed dropped correction context: `gthub`, `cmmit`, `psh`
  remained incorrect. After forwarding program, original field, surrounding
  text and selection, the actual scoped terminology pipeline returns complete
  `請檢查 GitHub、commit 與 push。`. Context is redacted from Debug output.
- Confirmed learning through the routed frontend initially failed as an unknown
  command. Session mapping and idle control handling have now been implemented;
  the regression checks the next actual dictation, not private stored state.
  Contract commands use an unreachable notification bus, so OS learning notices
  cannot appear on the user's desktop.
- This code remains unadopted by the desktop UI. Daily services/hotkeys are
  untouched; no live audio/input or GPU operation has been used. The remaining
  cancellation and process-owner work is recorded below.

- The confirmed-learning case passes: correcting `gthub` to `GitHub` in one
  acknowledged frontend session affects the next actual daemon dictation.
  Engine session IDs are translated for attribution; unacknowledged delivery
  cannot become learning evidence. Scalar no-change replies are not called saved.
- The rapid Stop/Cancel case failed because a final result beat a buffered cancel.
  Draining the bounded frontend command burst before engine events fixed it;
  the actual daemon test now attempts no delivery and retains no cancelled text.
  Lost delivery acknowledgement retains `Unconfirmed` text and ownership.
- Added the actual child owner: private profile/endpoint, explicit Nano CPU
  environment, profile lock and stop/wait cleanup. An attached dispatcher losing
  its frontend reaps even a provider that refuses to acknowledge cancellation,
  then releases the app so another provider may be selected. Setup sends no Start.
  Restart initially failed on a retained profile lock; shutdown now releases it.
- App exit without destructors initially left its engine running. A Linux
  parent-death signal and same-worker ownership fixed the real subprocess test.
  The fixture adopts/reaps its orphan and cleans its private socket directory.
  Three ownership tests and all 21 core cases pass (one subprocess helper is
  intentionally ignored except when invoked by the owner-exit case).
- All 10 actual app/daemon contracts pass with the OS notification bus isolated.
  Rebuilt the actual Nano feature, then ran the new Rust `probe_owned_local`
  against existing read-only assets in disposable data. Native sherpa/ONNX
  libraries loaded, GPU libraries were absent, no Start was sent and the child
  was reaped. No live speech/inference/latency acceptance is claimed.
- Next: connect the long-lived Tauri worker, report idle process death, enforce
  session deadlines, expose recovery text and prepare explicit frontend-owner
  migration. Runtime/model/spelling setup, shared vocabulary/review UI, Google
  and Windows integration remain. The goal is active; daily services are untouched.

- `bb6da91` engine/Fcitx CI passed. The Linux installed-UI job stopped before
  opening the app: tauri-driver's proxy accepted `/status` while its native driver
  still refused the TCP connection, causing an uncaught `RemoteDisconnected`.
  The saved driver log confirms the connection refusal. The existing bounded
  read-only condition loop now handles that transient disconnect and waits for
  the WebDriver `ready` flag; session creation/clicks are not replayed. Readiness
  semantics follow the [WebDriver status contract](https://www.w3.org/TR/webdriver2/#status).
  The native dispatcher/ownership tests passed in that run; UI checks must be
  rerun after this harness fix before recording a complete desktop CI pass.

### Resident app worker and recovery view

- Harness fix `8fca553` passed Linux `.deb` and Windows NSIS build/install/UI CI
  in [run 37524989608](https://github.com/Jadiouo/voicetype/actions/runs/37524989608).
  Both downloaded UI reports contain seven passing checks and explicitly record
  `speech_adapters_tested: false`. Actual file size/SHA-256 match their manifests:
  Linux 4,314,316 bytes, `4fd39514653c752a86bc37ba64c9e354a4da7e246b3931b0de4f6059ff249f35`;
  Windows 2,111,812 bytes, `abcab1c982c8af5de83cc1824285409f6bed7c79df81cd27d52b6e97110e2bd1`.
- TDD/B: a killed idle engine with a healthy frontend first remained usable for
  the entire observation window (**RED**). Health now checks the actual child,
  invalidates an active session if present and reaps before returning failure
  (**GREEN**). Keeping an old PID is no longer treated as health evidence.
- TDD/A,B: added a resident `DesktopWorker` through the UI-facing command seam.
  The initial test lacked the worker API (**RED**); the implementation now owns
  private Linux frontend listening, explicit native activation, routing, child
  health and shutdown on its spawning thread (**GREEN**). Activation sends no
  Start; a connected idle child remains alive after command completion. Killing
  that child changes status to Failed and removes the endpoint after cleanup.
- TDD/A,B: through the real worker/dispatcher and external process/input fixtures,
  a complete mixed-language final result is rejected by changed focus and retained
  for explicit recovery. The recovery commands were missing (**RED**); retrieval,
  reload preservation and session-matched dismissal now pass (**GREEN**). Busy
  switches are rejected; switching away while idle reaps the local child; recovery
  actions never resubmit text. Status JSON contains no transcript.
- Tauri commands now use this resident worker instead of owning Application in
  a mutex on transient blocking-pool jobs. Final app exit waits for its worker.
  Added a separate recovery section with a read-only full result, manual select/
  copy guidance, explicit dismissal and distinct uncertain/partial-delivery text.
  Webview polling cannot overwrite an intervening settings operation.
- Local verification: core suite **24 passed**, one subprocess-only helper
  ignored as intended; JavaScript/Python syntax and diff checks passed. Local
  Tauri check reached missing host GTK/Cairo development libraries, so it is not
  recorded as a build pass; actual Linux/Windows installer and UI validation for
  this new change must run in CI. No live mic, account login, input injection,
  discrete-GPU inference or production configuration was used.
- **Continuation:** phase deadlines, verified runtime/model setup with a concrete
  activation UI, frontend-owner migration/rollback, shared vocabulary/review and
  spelling setup, Google integration and Windows speech still remain. The current
  webview does not expose executable paths or call native activation; these are
  not yet working end-to-end speech installers. The goal stays active.

### Phase deadlines and UI cancellation

- `ad89d2f` passed [engine/Fcitx CI](https://github.com/Jadiouo/voicetype/actions/runs/37544476912)
  and [Linux/Windows installer/UI CI](https://github.com/Jadiouo/voicetype/actions/runs/37544476863).
  Downloaded artifacts match their source, size and SHA-256: Linux 4,454,074 bytes,
  `9023efa9c54d1ebefcadc29c36d22a4886eaf95689ecd4c8ccabb54232ab9875`;
  Windows 2,151,322 bytes,
  `f642c48f34af40660531167a0234ddfd08d90ef2c64e64ec7a33d8be155fb13c`.
  Both installed UI reports pass eight checks; actual screenshots show the
  recovery section on both platforms. Speech adapters remain explicitly untested.
- TDD/B: added a monotonic OS clock boundary and phase watchdogs: prepare 6 s,
  record 65 s, finalize 120 s, release/cancel 3 s. A stalled finalization now
  invalidates late output and keeps ownership until actual cleanup; repeated
  Stop/Cancel cannot renew its deadline. The first failure remains visible after
  subsequent transport errors. This adds no wait to successful dictation.
- TDD/B: the real owned-process test first showed that a child ignoring Cancel
  stayed busy after the deadline (**RED**). The dispatcher now enforces deadlines
  before polling and before accepting engine output; actual kill/wait precedes
  release (**GREEN**). No text is delivered or retained from that cancelled work.
- TDD/A,B: the UI-facing cancel operation initially did not exist (**RED**). It
  now invalidates the session through the resident worker (**GREEN**). A real
  external child that ignores cancellation is automatically reaped after 3 s
  without more frontend input. Tauri exposes the command and a busy-only cancel
  button; status distinguishes preparation, recording, finalization and release.
- TDD/B: an unexpected release while still recording originally showed no
  failure (**RED**); it now reports ProviderFailed and inserts no text (**GREEN**).
- Local core suite: **28 passed**, one deliberately ignored subprocess helper;
  native app/daemon contracts and the new installer/UI build are verified again
  below when the new source reaches CI. Daily services and private user data
  remain unchanged. Next concrete work is verified asset setup/activation, then
  frontend migration and the shared vocabulary/review/provider integrations.

### Verified asset storage and model provenance

- `91cc55c` passed [engine/Fcitx CI](https://github.com/Jadiouo/voicetype/actions/runs/37545231543)
  and [both desktop installer/UI jobs](https://github.com/Jadiouo/voicetype/actions/runs/37545231537).
  Downloaded source/size/SHA match: Linux 4,457,590 bytes,
  `4847659e2fb70da66f89a09e4d705df8ce273454e4a12af462cb2ea76b89a7a2`;
  Windows 2,154,596 bytes,
  `6127b10cce6c0b6f416f5a82482566e95d6a6f908e35384fbb2aa9d84a7d6f03`.
  Both UI reports pass eight checks and explicitly exclude speech acceptance.
- TDD/D: the initial installation test had no asset API (**RED**); a staged
  `AssetStore` now copies exact pinned regular files, verifies size/SHA-256,
  preserves previous versions and publishes a complete activation atomically
  (**GREEN**). Corrupt and incomplete updates keep the working version and
  sibling vocabulary unchanged. Upgrade/rollback checks passed on first run.
- An invalid previous activation record was initially overwritten (**RED**);
  bounded/schema/path validation now preserves it and rejects installation
  (**GREEN**). Reinstall initially required another source copy (**RED**); it now
  reuses a fully verified identical version, while a changed version is repaired
  into a new directory and cannot be selected for rollback (**GREEN**).
- Five asset cases pass locally. Added only the locked sha2 dependency chain;
  the daemon lockfile was updated for its dev dependency on the app core. These
  tests will run natively on Windows as well as Linux in desktop CI.
- Downloaded and verified the official GitHub Nano archive. All six model files
  match the existing daily model; a same-name Hugging Face export has different
  ONNX hashes and was not substituted. The pinned Silero upstream file also
  matches the daily VAD. Public catalogs contain URLs, versions, platforms, sizes
  and hashes only; binary downloads stay in ignored private storage.
- The public `probe_asset_install` example actually copied and verified the six
  Nano files and Silero into disposable stores. No inference, capture, user-data
  migration or service change occurred. Details, source links and license-notice
  limits are in [ASSETS.md](ASSETS.md).
- **Continuation:** wire bounded HTTPS/archive setup and progress/cancellation,
  native runtime manifests/relocatable packaging and explicit Tauri activation.
  Current asset storage takes a prepared source directory; it is not yet a GUI
  downloader. Frontend migration/rollback, shared vocabulary/review/CSC, Google
  and Windows speech remain. Keep the full goal active.

- Asset source `f17c6f6` passed [engine/Fcitx CI](https://github.com/Jadiouo/voicetype/actions/runs/37546329991)
  and [Linux/Windows desktop CI](https://github.com/Jadiouo/voicetype/actions/runs/37546330011).
  The native Windows and Linux logs each explicitly show all five asset cases
  passing. Both installed UI reports pass eight checks with speech untested.
  Downloaded packages match their source, sizes and SHA-256: Linux 4,453,498 bytes,
  `304c79c9a04b0b26b4ee11e5510dbccd7d4115852af3117ad398a3ca705dd4b2`;
  Windows 2,155,217 bytes,
  `a6c302d3e6c2267f09928fa89e8faf481df4b9693c1b7b7e3152f8974f11185b`.
  All 185 remote source blobs were independently matched to the local commit.
  This verification does not change the remaining setup/speech scope above.
