#!/usr/bin/env bash
# Build a reviewable optional profile, never overwrite/install the live daemon.
set -euo pipefail
repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
nano_native_root="${VOICETYPE_SHERPA_NATIVE_ROOT:-$repo_dir/private/runtime/sherpa-nano-integrity-1.13.8}"
if [[ ! -d "$nano_native_root" ]]; then
    echo "Missing pinned native CPU artifacts: $nano_native_root" >&2
    exit 1
fi
nano_native_root="$(cd -- "$nano_native_root" && pwd -P)"
nano_target_dir="$(realpath -m -- "${VOICETYPE_NANO_TARGET_DIR:-$nano_native_root/target}")"
normal_target_dir="$(realpath -m -- "$repo_dir/voicetyped/target")"
case "$nano_target_dir" in
    "$normal_target_dir"|"$normal_target_dir"/*|*/voicetyped/target|*/voicetyped/target/*)
        echo "Refusing to reuse the normal daemon Cargo target directory or its descendants." >&2
        exit 1
        ;;
esac
export VOICETYPE_SHERPA_NATIVE_ROOT="$nano_native_root"
export CARGO_TARGET_DIR="$nano_target_dir"
# The build script independently pins the public header/C-API/ORT hashes.
flock /tmp/voicetype-evaluation.lock cargo build \
    --manifest-path "$repo_dir/voicetyped/Cargo.toml" \
    --release --features sensevoice,sherpa-nano
echo "Candidate built only: $CARGO_TARGET_DIR/release/voicetyped"
echo "Not installed; see docs/NANO-PREVIEW.md for validation status."
