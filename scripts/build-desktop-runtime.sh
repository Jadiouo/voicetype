#!/usr/bin/env bash
# Prepare an isolated Linux CPU bundle; never install/restart a user's service.
set -euo pipefail
if [[ $# != 2 ]]; then
    echo 'Usage: build-desktop-runtime.sh VERIFIED_NATIVE_ROOT NEW_BUNDLE_DIRECTORY' >&2
    exit 2
fi
repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
native_root="$(realpath -e -- "$1")"
bundle_dir="$(realpath -m -- "$2")"
target_dir="$repo_dir/private/desktop-runtime-build"
if [[ -e "$bundle_dir" || -L "$bundle_dir" ]]; then
    echo 'The bundle destination already exists; choose a new directory.' >&2
    exit 1
fi
# build.rs verifies header/library hashes; this feature does not relax the pins.
VOICETYPE_SHERPA_NATIVE_ROOT="$native_root" CARGO_TARGET_DIR="$target_dir" \
    nice -n 10 cargo build --manifest-path "$repo_dir/voicetyped/Cargo.toml" \
    --locked --release --no-default-features --features relocatable-runtime -j 2
mkdir -p -- "$(dirname -- "$bundle_dir")"
staged_dir="$(mktemp -d -- "$(dirname -- "$bundle_dir")/.runtime-XXXXXXXX")"
trap 'rm -rf -- "$staged_dir"' EXIT
mkdir -- "$staged_dir/bin" "$staged_dir/lib"
install -m 755 -- "$target_dir/release/voicetyped" "$staged_dir/bin/voicetyped"
for library in libsherpa-onnx-c-api.so libonnxruntime.so; do
    install -m 644 -- "$native_root/lib/$library" "$staged_dir/lib/$library"
done
python3 "$repo_dir/desktop/tests/runtime_layout.py" "$staged_dir"
# This helper prepares runtime bytes for further packaging. License/provenance
# assembly, clean-machine dependency installation and Tauri activation are
# separate release steps; this directory is not a distributable installer.
mv -T --no-clobber -- "$staged_dir" "$bundle_dir"
if [[ -d "$staged_dir" ]]; then
    echo 'The destination appeared during the build; it has been preserved.' >&2
    exit 1
fi
echo "Prepared only: $bundle_dir"
