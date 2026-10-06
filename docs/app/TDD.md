# VoiceType Desktop — Test Design and TDD Plan

Status: proposed public test boundaries, awaiting the user's seam agreement.
Existing Linux behavior was already tested before PR #1 merged. Do not rerun that
entire suite merely to merge or create this shell.

## Public boundaries to agree before writing new tests

| Seam | Observable user behavior | Requirements |
| --- | --- | --- |
| A. Application commands used by the UI | Choose Local/Google, reopen and keep the choice, report actual setup status, reject a busy switch | APP-01, 02, 03, 07 |
| B. Session/provider and OS delivery boundary | Stop/cancel, ignore late or duplicate results, preserve text and original target; never switch to Google implicitly | APP-02, 03, 04, 09 |
| C. Vocabulary/review commands | Edit explicit terms, preview corrections, confirm/correct samples, promote a rule, enforce expiry and daily limit | APP-04, 05 |
| D. Installer/upgrade entry points | Clean install, missing dependencies, interrupted assets, restart, upgrade/rollback and uninstall preserve user data | APP-06, 07, 08, 10 |

Tests call these public interfaces. They do not assert private method calls,
widget layout, internal data structures or numbers copied from the implementation.
Use real temporary files; substitute only system boundaries such as OS focus,
capture, clock or official CLI process. No live microphone, account login, real
input injection or GPU is needed for offline tests.

## Red → green sequence

One scenario and minimal implementation at a time. Record the failure before the
implementation and the passing result afterward in the development log.

1. **M1/A:** New user sees both providers, Local is selected, neither is claimed
   ready without its runtime. Save Google; recreate the application through its
   public open command and observe Google selected.
2. **M1/A:** Reject unsupported/corrupt configuration without overwriting it;
   preserve a concurrent edit and existing sibling vocabulary/review files.
3. **M2/B:** Start one ready provider, reject a busy switch, stop and accept one
   final result; cancelled or previous-session results cannot be delivered.
4. **M2/C:** Vocabulary save/preview and review actions through the same commands
   used by the app. Preserve existing Linux formats and unknown fields.
5. **M2/D:** Install staged runtime in a temporary user home; interrupted or
   hash-mismatched downloads leave the last working runtime active.
6. **M3/B,D:** Windows native process/capture/input adapters, then Windows clean
   install and preservation tests. CI platform compilation alone does not satisfy
   microphone, focus, login or target-app acceptance.

Each next case is selected from the preceding result. Do not bulk-write all tests
against a hypothetical implementation. Formatting/build checks are separate from
behavioral test evidence.

## Acceptance matrix

| Scenario | Linux | Windows | Evidence required |
| --- | --- | --- | --- |
| Shell starts from installed entry; tray reopens one window | Required | Required | Installed artifact, process and visible window |
| Local/Google preference survives restart | Required | Required | Public command tests + shell interaction |
| Missing model/CLI/login is actionable, never fake Ready | Required | Required | Negative setup cases |
| Local recording fully offline, no discrete-GPU inference | Required | Required | Real runtime/device inspection |
| Google official login/voice/stop/editor capture | Required | Required | Real CLI session; no prompt submission |
| Traditional Chinese + English + final proper noun | Required | Required | Matched complete utterance/output; retain pauses |
| Capture begins under 1 second when warm | Required | Required | Defined key event to capture-ready measurement |
| Stop latency does not regress from the same provider baseline | Required | Required | Paired trials; separate drain/inference/correction/delivery |
| Cancel/timeout/disconnect, switch busy, duplicate/stale result | Required | Required | Offline commands plus platform smoke |
| Focus changes/destroyed target, password/elevated input | Required | Required | No delivery to a new/unsafe target; visible undelivered text |
| Vocabulary persistence/conflicts, sampled review, expiry | Required | Required | Shared commands + actual platform file operations |
| Clean install/update/rollback/uninstall preserve data | Required | Required | Temporary user profile, files read through public interfaces |

A smaller positive sample is adoption evidence for that build, not a claim of
general speech accuracy. The old Google catchup trials are not automatically
Windows or packaged-app performance results.

## Release evidence

- CI builds the actual Linux `.deb` and Windows NSIS `.exe`, with locked dependencies.
- Artifacts include version/commit, checksums and required notices; user data and
  credentials never enter build inputs.
- Downloaded artifacts are inspected, not merely inferred from green CI.
- Development preview clearly states which adapters are not connected.
- Production release stays blocked until every required platform row has evidence;
  an unavailable Windows desktop is recorded as missing evidence, never as a pass.
