# Local engine desktop protocol v1

This opt-in extension supports the desktop coordinator. It is not yet installed
in the daily service. The existing Unix NDJSON endpoint and legacy requests stay
compatible. A desktop adapter must negotiate before sending Start; old daemons
ignore unknown requests and must be reported incompatible after a bounded timeout.

## Read-only negotiation

Request: `{"type":"desktop_status","request":1}`

Reply:

```json
{"type":"info","value":{"desktop_protocol":1,"request":1,"capabilities":["session_events","suspend"],"session_busy":false}}
```

Correlate the request ID and validate version/capabilities. This reports protocol
support and outstanding session work, not microphone permission, model provenance
or desktop input readiness. It does not open a microphone.

## Session lifecycle

Add `"session_events":true` to the existing Start request, preserving session,
program, password flag and context fields. The daemon sends
`{"type":"state","session":17,"value":"recording"}` after capture begins.
Stop/Cancel keep their existing format. A complete Result or Error is followed by
`{"type":"state","session":17,"value":"idle"}` once the session work ends.
Cancelled in-flight inference emits Idle only after its native work finishes;
its late text is suppressed. Ignore state/results for a different session.

Idle is not a microphone-close acknowledgement: the normal warm stream can
remain open for its idle grace period. The adapter must handle session cleanup
and microphone handoff as separate operations. Legacy clients omitting
`session_events` keep their prior result stream.

## Microphone handoff

Request: `{"type":"desktop_suspend","request":2}`

Reply:

```json
{"type":"info","value":{"desktop_protocol":1,"request":2,"microphone":"closed"}}
```

`closed` is sent after the audio thread drops the idle stream and ring. `busy`
means recording, preparation or inference is still outstanding; `error` means
cleanup could not be confirmed. Never open the other provider on `busy`, `error`,
timeout or disconnection. The OS audio boundary also rejects suspending an active
capture. Ordinary same-provider sessions keep the existing warm behavior.

The application must exclusively own its provider runtime before using this as
a handoff contract. This protocol does not by itself stop another legacy client
from starting a new recording after the acknowledgement. Migration must transfer
the input integration owner; do not point a candidate coordinator at daily
hotkeys while both integrations can record.

## Evidence and remaining integration

`voicetyped/src/desktop_ipc_tests.rs` exercises the actual socket/server/session
and text pipeline, substituting only OS capture and the native ASR boundary.
Capture tests exercise the public suspend call and real audio-thread command
handler with an OS-stream fixture. `desktop/tests/local_engine_probe.py` can load
the real Nano CPU runtime with disposable settings and send only status, suspend
and ping. It verifies native libraries, CPU startup and clean exit without Start.
These tests do not establish real microphone or target-application acceptance.

`desktop/core/src/local.rs` implements the app-side connection on Linux. It
requires the caller's owned child PID, authenticates UID/PID with `SO_PEERCRED`
before transmitting, and negotiates capabilities. It is blocking native-worker
code with bounded reads/writes, not webview-thread code. Partial frames survive a
poll timeout; malformed/oversized frames or disconnection fault the connection.
An uncertain write is not reported as a rejected command: the app retains the
session and its owner must stop/reap that child before reporting release. No
transport error synthesizes idle or authorizes another provider's microphone.

`LocalDispatcher` now routes the real frontend/app/engine stream. Start carries
program, original input context, surrounding text and selection. Frontend and
engine session IDs are mapped explicitly, including confirmed learning from the
last acknowledged delivery. Queued Stop/Cancel commands are drained before a
fast final result. A changed correction replies `correction_saved`; a scalar
false engine response is only `correction_unchanged`, not a claim of persistence.
Unknown attribution or a control error is `correction_rejected`.

The Tauri worker, frontend endpoint/migration, recovery UI and verified asset
setup remain to be connected. Google and Windows adapters require their own
equivalent contracts and platform evidence.

## Fcitx app delivery (v1)

The app owns a private frontend endpoint after an explicit migration. Before
accepting shortcuts, send `{"type":"desktop_hello","session":NONCE}` and require
`{"type":"desktop_hello","session":NONCE,"value":"voicetype.fcitx.v1"}`. This
handshake neither records nor changes shortcuts; old addons do not acknowledge.
A connection/reconnection has its own session identity; never reuse a target
lease after reconnecting.

The frontend's Start contains its session ID and `context_id`. Keep those original
values separate from the provider session ID. To deliver, send:

```json
{"type":"deliver","session":1,"context_id":"original-context","text":"final text"}
```

Fcitx replies with type `delivered`, the same session and context, and a `code`:

- `committed`: validated the original weak input context, focus generation and
  sensitivity, rechecked after preedit callbacks, then called `commitString`.
- `stale`: that frontend session is no longer pending, including duplicate requests.
- `focus_changed`: context mismatch, target loss or reentrant focus/session change.
- `invalid_text`: empty, oversized, NUL-containing or invalid UTF-8 text.

Pending delivery is consumed before callbacks. These app requests never invoke
legacy clipboard fallback; the app retains undelivered/uncertain text for recovery.
An acknowledgement only establishes the Fcitx operation, not visible acceptance
by an external application. A lost acknowledgement is uncertain; never retry the
complete text automatically. The old daemon `result` path remains compatible.

The isolated Fcitx harness covers one commit, duplicate acknowledgement, wrong
context, focus-out/return, preedit focus changes and weak-target destruction. It
runs without a live display, bus, microphone or user config. Separate actual
app/daemon tests exercise the native dispatcher and input acknowledgement with
an OS frontend fixture. The Tauri shell now uses the resident application worker
and has a separate recovery view. Linux now exposes explicit verified runtime
loading. Input-owner migration and actual target-application acceptance remain
outstanding.

## Owned Linux runtime

`OwnedLocal` starts a reviewed native executable with explicit Nano/CPU model
paths, private engine endpoint and separate data profile. A stable profile lock
prevents two app owners from launching into the same profile. Preparation only
negotiates status; it does not attach a frontend or send Start. Environment
inheritance is limited to OS audio/notification access. Arbitrary provider,
screen-helper, library-path and model flags are not inherited. Shared spelling
worker setup and data migration must be explicitly configured before adoption;
preparation alone does not enable the existing daily spelling service.

`OwnedLocalSession` joins this process handle to the dispatcher. On IPC failure,
results are invalidated, the actual child is stopped and reaped, then the app
session is released. A failed cleanup never authorizes another recording.
Explicit shutdown also reaps the process and releases its profile lock.

The child requests a Linux parent-death signal before exec. The owner is `!Send`
and stays on the long-lived native worker that spawned it, because this signal
tracks the creating thread, not just the process as a whole. A process-exit
regression deliberately skips Rust destructors and verifies the child terminates;
its test subreaper also reaps the orphan. See the Linux
[PR_SET_PDEATHSIG contract](https://man7.org/linux/man-pages/man2/PR_SET_PDEATHSIG.2const.html)
and Rust's [pre_exec safety requirements](https://doc.rust-lang.org/std/os/unix/process/trait.CommandExt.html#tymethod.pre_exec).
The installer must provide a normal, reviewed, non-privileged executable.

`probe_owned_local` (a Rust example in `desktop/core/examples/`) prepares the
actual Nano runtime with disposable data, checks loaded native libraries and
absence of GPU libraries, sends no Start, then verifies the owned child was
reaped. This is process/control evidence, not microphone or latency acceptance.

`DesktopWorker` creates, polls and destroys its owner on one resident thread.
An explicit native activation command prepares the runtime and a mode-0700
private frontend endpoint, then authenticates one frontend before marking it
connected. `try_wait` detects idle crashes even when no input command arrives.
Switching away from Local while idle first reaps the local owner. Busy switches
are rejected. The worker's request queue is bounded; commands are not retried on
an ambiguous request timeout. Tauri's blocking pool performs setup asset checks
and waits on request/reply; runtime ownership stays on the resident worker. The
final application exit waits for worker shutdown.

Recovery text is requested separately from diagnostic status. The last retained
result survives preference reload until an explicit session-matched dismissal or
app exit. Session IDs cross the JavaScript boundary as opaque strings. Recovery
does not inject text or change the clipboard. An uncertain acknowledgement says
the text may already have been committed; users can select and copy manually.

Session deadlines use a monotonic clock: capture preparation 6 seconds, recording
65 seconds (the engine normally enforces its own 60-second limit), finalization
120 seconds and release/cancellation 3 seconds. These are failure watchdogs,
not delays before sending valid text or changes to the speed acceptance target.
Repeated Stop/Cancel and stale state events do not extend a phase. The dispatcher
checks before polling and before accepting a returned result. Expiration marks
TimedOut, invalidates text and requests one cancellation; only actual owned-child
cleanup releases the app. The first failure is preserved when a later transport
failure occurs. Unexpected release without a result/cancel/error is itself a
visible provider failure.

The settings UI can cancel through the resident worker and displays preparation,
recording, finalization and release separately. A non-acknowledging child is reaped
at the release deadline without another frontend command.

The Linux load command now verifies installed model catalogs and copies bundled
runtime assets using a catalog embedded at app build time, then activates this
native owner. No webview command accepts executable/model paths, catalogs or
URLs. Opening the preview creates only the worker; it does not start a provider
or alter daily input integration. Frontend-owner migration is still pending.
