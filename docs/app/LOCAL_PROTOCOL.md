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

The application/native dispatcher and owned runtime supervisor are still to be
connected. Start currently forwards only the session/target; program and bounded
surrounding/selected text must be carried through the dispatcher before adopting
it for daily correction/learning. Google and Windows adapters require their own
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
runs without a live display, bus, microphone or user config. Actual app routing,
recovery UI and target-application acceptance remain outstanding.
