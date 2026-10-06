# Correction capture regression checks

```sh
cmake -S voicetype-fcitx5 -B voicetype-fcitx5/build -DCMAKE_BUILD_TYPE=RelWithDebInfo
cmake --build voicetype-fcitx5/build -j4
ctest --test-dir voicetype-fcitx5/build --output-on-failure
```

`autolearn_*` dispatches real Fcitx key/surrounding-text/focus events through the
addon and observes actual Unix IPC messages. Its text frontend and ASR peer are
synthetic; it does not access the installed daemon, microphone or user text.

The original regression is commit → edit → wait 400 ms → Return → field clear →
next dictation. Before the fix, `autolearn_submit` fails with zero corrections.
The addon previously checked only at the next dictation, after the edited value
had disappeared. It now preserves the observed value and can finalize it when
the same field removes exactly the tracked insertion, keeping both outer anchors.

The suite also covers Ctrl+Return, a same-field clear without a key (the mouse
submit event pattern), duplicate clear updates, ordinary next-dictation capture,
and one selected replacement reported as deletion then insertion. A Return key
followed by an exact clear permits a 50 ms edit; Return alone or a newline never
confirms learning. Automatic messages remain `confirmed=false`, so the daemon's
two independent observations policy is unchanged. Ctrl+Caps Lock remains the
separate explicit-confirmation path, including after a pasted correction.

Negative cases cover missing edit evidence, a subsequent unkeyed app rewrite,
clipboard shortcuts, undo, Escape, select-all/delete, focus changes, expiration,
active composition, and a mouse-like clear before the 350 ms stability window.
Each direct ASCII key predicts the full next text and collapsed caret from the
actual pre-edit cursor/selection. `rawKey` preserves the intended uppercase or
shifted character; normalized shortcut keys are not text predictions. Backspace
and Delete predict exactly one ASCII scalar or the exact ASCII selection.
A selected literal may produce two updates, but only its exact deletion followed
by that one literal is accepted. Every later character needs its own key and
acknowledgement. A second key before acknowledgement, unsupported modifiers,
virtual/forwarded keys, preedit, invalid offsets, changed caret or an unrelated
first update discards automatic attribution. Manual Ctrl+Caps Lock remains.

The `keypress_app_rewrite` regression sends x at the end while the application
suppresses x and changes a word elsewhere. It used to send an observed correction
because only the recent key timestamp was checked; now it sends none. Positive
fixtures use real per-character replacement or deletion/insertion sequences,
including uppercase/Caps Lock and Unicode text around the ASCII edit. They no
longer simulate an entire word replacement after one arbitrary Backspace or G.

Limits: exact key/delta agreement is consistency evidence, not proof of human
authorship. Fcitx cannot distinguish an application that generates exactly the
same observable edit. Chinese IME composition, unknown grapheme operations and
batched text updates require explicit confirmation. The same-field clear proves
the prior text disappeared, not that a server received a message. Tests therefore
establish bounded attribution and one pending observation, not perfect intent
recognition. A mouse submit that first causes focus-out/reset is deliberately
discarded; a fast mouse clear without a submit key still needs 350 ms of stable
text. Actual Electron/browser SurroundingText availability and event sequencing
must be checked independently; these tests do not claim that compatibility.

## Explicit selection fallback

When SurroundingText is unavailable, **Ctrl+Caps Lock only** invokes
`scripts/context-selection.py` through a private pipe. The installer places it at
`~/.local/bin/voicetype-selection`; Ubuntu dependencies are `python3-gi`,
`gir1.2-atspi-2.0`, and `x11-utils`. No background observer is started. Tests may
supply a trusted helper path using `VOICETYPE_SELECTION_HELPER`.

The helper matches the active X11 window PID to exactly one AT-SPI application,
checks the Fcitx program against the executable, and requires one fresh ACTIVE
frame with one focused, enabled, showing, editable entry/text node. It reads only
one explicitly selected range (at most 512 Unicode characters), twice to detect
changes. It never reads PRIMARY, clipboard, accessible names, window titles, or
unselected page text. Collection queries limit long-page focus lookup to two
candidates; providers without Collection use a bounded 512-node metadata walk.
Password/unknown roles, ambiguous windows, changed focus/selection, excessive
page size and timeout produce a visible refusal, not a learning success.

Chromium can enable its accessibility tree on demand via targeted frame
`get_attributes` / `get_relation_set`; those return values are discarded. The
helper makes one bounded attempt when no editable is exposed. This is neither a
global accessibility setting change nor an application restart. It does not
assume the same behavior on every Electron release. The original dictation did
not capture an AT-SPI field identity: consequently this fallback is **manual
confirmation**, never evidence for automatic learning. The addon still requires
the same Fcitx context, focus generation, program and unexpired delivered session;
the daemon independently validates the sentence difference.

`selection_*` tests cover actual Fcitx event-loop/Unix IPC dispatch with a fixture
helper and no SurroundingText, duplicate shortcuts, refusal, timeout, focus loss,
and mismatched program. Python privacy tests cover exact range reads, cache
refresh, password/role rejection, Collection ambiguity, large pages and targeted
AX activation. `selection_reader` launches real subprocesses (40 fast write/exit
runs), checks output limits, verifies timeout/cancellation kill the owned process
group and reap the child, and measures that UI heartbeats continue. Timer accuracy
is explicit; Fcitx's default coalescing must not stretch the 500 ms deadline.

## ASR delivery errors

`asr_*`, `stale_error`, and `control_error` use the real IPC/event loop to check
that current-session ASR failures visibly say `語音輸入未送出`, with bounded,
markup-escaped text. Auxiliary-panel fallback preserves existing auxiliary text.
There is no committed text or correction message after the failure. Stale errors
cannot cancel a newer result; learning/control errors with no delivery target do
not duplicate daemon notifications. Empty audio remains quiet.

## Ctrl+Caps Lock on native X11

Swallowing the shortcut does not itself prevent XKB from changing Caps Lock.
The addon preserves the original **raw** Lock bit on press and physical release;
Num Lock and other modifiers are untouched. It only operates when the context's
`x11:` display is present in Fcitx's XCB module and verified as non-XWayland, with
a physical keycode and timestamp. Missing metadata, native Wayland and XWayland
leave keyboard state alone; this is not a keyboard remapping feature.

Fcitx consumes core XKB StateNotify before addon event filters. A dedicated XKB
connection is therefore opened only for that verified display, with its version
handshake during initialization. During an identified shortcut it subscribes to
modifier-state events for at most two seconds. Selection and Lock-only requests
use the same connection, so they remain ordered without a key-path round trip.
A matching physical release can finish after focus loss or IC destruction;
programmatic Lock requests cannot. The subscription ends after release or lease
expiry. Expiry discards saved state and never performs a late write. Ordinary
Caps presses remain ordinary; repeat does not replace the saved value or confirm
learning twice. Core XKB keycodes do not distinguish two physical keyboards.

A hold longer than two seconds, loss of the X connection, remapped Caps behavior,
or a frontend without original keycode/time can still leave Caps Lock changed.
Wayland compositors own their modifier state, so this X11 method is deliberately
not applied there. Real GTK3/Fcitx DBus behavior was checked in an isolated Xvfb;
that is not a compatibility claim for every Chromium/Electron frontend.

Build dependencies are Fcitx5's XCB development module, `libxcb-xkb-dev`, and
`libxcb-ewmh-dev`. CMake explicitly prints ENABLED or warns DISABLED when these
are absent; `-DVOICETYPE_X11_CAPS_RESTORE=OFF` produces the intentional stub. The
native test additionally needs X11/XTest libraries and `Xvfb` (an explicit
`-DXVFB_EXECUTABLE=/path/to/Xvfb` is supported). Its launcher creates its own
X server and refuses the inherited display. It checks Lock initially on/off,
both release orders, missing frontend release, IC teardown, immediate release
without an event-loop gap, Num Lock, ordinary Caps, repeat, unknown display/code,
queued older gestures, observer disconnection, and expiration. The always-available
lease unit test also checks timestamp wrap,
wrong display/keycode, duplicate release and stale timer deadlines.
