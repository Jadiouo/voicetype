# Goal: complete VoiceType for Linux and Windows

Status: paused at the user's request on 2026-10-08. Resume only when requested.
The full application remains incomplete; a settings preview is not completion.

Deliver installable Linux `.deb` and Windows `.exe` applications with selectable
Local Nano CPU and Google official Antigravity CLI dictation. Both share
Traditional Chinese conversion, English/name protection, editable vocabulary,
bounded automatic spelling correction and optional local sampled review (up to
five recordings per day, seven-day retention).

Complete capture, configurable shortcuts, stop/cancel, focus-safe delivery,
duplicate-result protection, tray/settings, runtime/model setup, official login
guidance, upgrades and rollback while preserving user data. Keep speed first;
never implicitly switch local audio to a cloud provider or use CUDA/discrete-GPU
inference. No paid API fallback.

Follow [SDD](SDD.md) and the four already-confirmed [TDD](TDD.md) boundaries. Work
continues through implementation, actual package verification and platform
acceptance. CI compilation, mocked engines and settings-only tests cannot satisfy
end-to-end speech acceptance. If an interactive Windows login/recording check
requires the user, finish independent work first and provide precise minimal steps.
Record missing evidence honestly; do not label the goal complete prematurely.

Keep [DEVLOG](DEVLOG.md) current at meaningful milestones and push reviewed,
privacy-checked code to the existing app PR. Do not publish private speech samples,
personal dictionaries, credentials or private development history. Preserve the
working daily installation until a verified migration and rollback exist.
