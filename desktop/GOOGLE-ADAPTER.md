# Official Google CLI adapter status

VoiceType uses the [official Antigravity CLI](https://antigravity.google/docs/cli/install/)
and the user's own official account. It does not call a paid speech API, copy
account data, or submit a prompt to the agent. The CLI is installed and logged
into separately by the user; VoiceType does not bundle or update it. The daily
voice installation remains independent.

The native adapter verifies an exact reviewed CLI image before launching an
interactive PTY (Linux) or ConPTY (Windows). A matching file is only an
installation check. Login, workspace trust and editor access require a live,
empty Ctrl+G editor capture before any microphone starts. An incompatible
version never starts capture. Official [voice control](https://antigravity.google/docs/cli/commands/voice/)
uses F5, and [external editing](https://antigravity.google/docs/cli/using/)
uses Ctrl+G. No adapter API can send Enter or prompt text.

On Linux, the adapter owns a native PipeWire recorder and an authenticated,
bounded in-memory PCM relay. It captures recorder stdout to EOF, confirms every
relayed byte and waits for the official recorder pipe to empty before one F5
stop. It then waits for recorder exit, takes two unchanged editor snapshots
separated by 200 ms, checks the official log for known voice errors, reaps its
children and only then offers the second draft to the common provider and
target coordinator. The normal relay pace is 16 kHz mono s16 real time even
after the official recorder pauses its reads. Catch-up is an explicit opt-in,
defaults off, and uses at most 2× rate after stop when queued audio reaches
200 ms; it returns to 1× below 100 ms. The remaining worker wiring
must feed its result through the existing vocabulary, Traditional Chinese
conversion, optional CPU spelling and focus-protected delivery path.

The Linux path has a synthetic recorder/CLI end-to-end test that compares PCM
produced with bytes consumed, plus tests for cancellation, one stop, two
captures, error rejection, stalled-reader pacing and cleanup ownership. These tests do not establish
speech quality or live CLI compatibility. A no-microphone check of official
1.3.1 confirmed its version and `--log-file`; an isolated, unauthenticated
TUI session did not reach the editor callback. The already used Linux 1.2.12
flow is the compatibility baseline. Neither binary is changed by this branch.

The Windows process owner attaches the ConPTY child to a kill-on-close Job
Object before ready; CI has a synthetic ConPTY/editor fixture. Windows CLI
microphone PCM drain and its native stop behavior have not been verified, so
Windows Google voice remains unavailable until that native path and a Windows
11 user trial pass. The current settings UI also does not activate the Linux
adapter. The preview must continue to label Google as unavailable until its
input/worker wiring and live acceptance are complete.

For a later candidate, the minimum user-operated trial is: install/login with
the official CLI instructions, grant its voice permission, select Google in
VoiceType, focus a disposable text field, record one Traditional Chinese
sentence with an English term, release once, and verify exactly one insertion
at the original field with no agent prompt submission. Repeat once after
changing focus to check recovery. Windows 11 needs the same trial on the
actual machine. A version or packaging check cannot replace it.

Dependency notice: [`portable-pty`](https://github.com/wezterm/wezterm)
0.9.0 is MIT-licensed. The official CLI and system PipeWire recorder are not
redistributed by this adapter. The PCM/editor implementation in this branch is
new Rust code informed by observed public CLI controls and the existing daily
workflow; no private recording, credential, or external repository source is
included.
