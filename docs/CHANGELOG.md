# Development changes

## Unreleased

- Optional pinned Nano CPU recognition, completion checks, full-utterance failure handling and bounded segmentation. Includes the upstream integrity patch and shim contract tests.
- Warm capture stays open throughout active recording; its 30-second idle timer starts after end/cancel.
- Fcitx delivery tracks a live pending input context, discards stale results and handles cancellation, disconnection and destroyed contexts.
- Configurable correction shortcut, explicit selection helper, native X11 Caps Lock preservation and tighter attribution for observed keyboard edits.
- Literal/name protection, conservative learned-rule ambiguity handling, shared output policy and isolated chronological replay tools.
- Live vocabulary reload and optional bounded CPU Chinese spelling correction.
- GTK vocabulary settings, tray icon, safe TOML editing, saved-text preview and login startup controls.
- Opt-in local sampled review: up to five longer utterances per day, seven-day retention, playback, human corrections and explicit vocabulary promotion.

The repository includes source, tests, general setup instructions and synthetic
fixtures. Models, recordings, personal dictionaries, deployment backups and
private evaluation histories are kept outside the public history. Demo corpora
exercise tooling; they are not a representative accuracy benchmark.
