# VoiceType desktop development preview

This branch starts the Linux/Windows application described in
[SDD](../docs/app/SDD.md) and [TDD](../docs/app/TDD.md).

**This preview saves the Local/Google preference. It does not yet record, switch
the existing dictation services or bundle speech models.** Keep using the existing
Linux installation for dictation, vocabulary and review until the adapters pass
the staged acceptance gates.

"Check services" sends a read-only ping to an existing Linux local engine and
distinguishes response, missing service, timeout and incompatible reply. It does
not start audio or establish Google login readiness. The shared core now also
tests provider/target ownership, cancel, late/duplicate results and failed input
delivery; native recording/input adapters are still being connected.

The preview has its own application ID and settings directory:
`io.github.jadiouo.voicetype.preview`. It never imports or changes the existing
`voicetype` settings. Closing its window exits the preview; a desktop tray entry
also opens settings where the desktop supports it. No autostart is installed.

## Development

Use Rust 1.90+ and Node 22+. For native prerequisites, follow
[Tauri's platform instructions](https://v2.tauri.app/start/prerequisites/).

```sh
cd desktop
cargo test -p voicetype-app-core --locked
npm ci --ignore-scripts
npm run tauri -- dev
```

Build on the target OS:

```sh
# Linux
npm run tauri -- build --ci --bundles deb -- --locked
# Windows
npm run tauri -- build --ci --bundles nsis -- --locked
```

Artifacts are under `desktop/target/release/bundle/`. GitHub Actions builds both
platforms and uploads development artifacts with their source commit and hashes;
it does not publish a production release. Windows previews are unsigned and their
installer may show the normal Windows publisher warning. No certificate is
silently obtained and no signing secret is stored in the repository.

The static UI is bundled locally, has no remote scripts, and cannot directly run
shell commands or access arbitrary files. Only typed Rust commands expose settings.
Core tests use temporary directories; no microphone, live dictation, GPU, account
login or text injection is part of preview validation.

The CI UI check runs the installer payload with a real platform WebDriver, chooses
Google, reopens the app, and checks error/reload behavior. To use a disposable
profile, set `VOICETYPE_PREVIEW_CONFIG_DIR` to an absolute temporary directory.
This overrides only the preview's configuration directory; the setting is never
used by the production dictation daemon. UI evidence is saved separately from
installer artifacts. Neither is proof of working speech adapters.
