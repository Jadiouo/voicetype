# Goal: merge the Local VoiceType desktop preview

Status: local-only preview integration. On 2026-10-11 the user withdrew Google
from this App and asked to merge the reviewed worktree promptly. The former
full dual-provider goal is paused; this preview is not a claim of complete
Windows dictation or live-speech acceptance.

Deliver installable Linux `.deb` and Windows `.exe` previews using Local Nano
CPU dictation only. Shared Traditional Chinese conversion, English/name
protection, editable vocabulary, bounded automatic spelling correction and
optional local sampled review remain. Existing preview settings selecting Google
migrate to Local on startup or reload without removing other user data. The App
no longer bundles or downloads Google helpers or the official CLI. An unfinished
old shortcut transfer retains an explicit restore action; it does not activate
Google recognition.

Complete capture, configurable shortcuts, stop/cancel, focus-safe delivery,
duplicate-result protection, tray/settings, runtime/model setup,
upgrades and rollback while preserving user data. Keep speed first;
never implicitly switch local audio to a cloud provider or use CUDA/discrete-GPU
inference. No paid API fallback.

Follow [SDD](SDD.md) and the four already-confirmed [TDD](TDD.md) boundaries. Work
continues through implementation, actual package verification and platform
acceptance. CI compilation, mocked engines and settings-only tests cannot satisfy
end-to-end speech acceptance. If a live Windows recording check requires the user, finish independent work first
and provide precise minimal steps.
Record missing evidence honestly; do not label the goal complete prematurely.

Keep [DEVLOG](DEVLOG.md) current at meaningful milestones and push reviewed,
privacy-checked code to the existing app PR. Do not publish private speech samples,
personal dictionaries, credentials or private development history. Preserve the
working daily installation until a verified migration and rollback exist.
