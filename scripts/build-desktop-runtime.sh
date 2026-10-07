#!/usr/bin/env bash
# Prepare an isolated Linux CPU bundle; never install/restart a user's service.
set -euo pipefail
if [[ $# != 2 && ( $# != 3 || "$3" != '--source-built' ) ]]; then
    echo 'Usage: build-desktop-runtime.sh VERIFIED_NATIVE_ROOT NEW_BUNDLE_DIRECTORY [--source-built]' >&2
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
# Do not inherit a caller's pin. Fresh release builds explicitly attest their
# reviewed recipe and output here; the legacy local helper keeps its old pin.
unset VOICETYPE_SHERPA_CAPI_SHA256
capi_sha='72408cc5f2407eb0ba46cd381614229107f225b8ccc4149e2f5e4b09957834dd'
if [[ $# == 3 ]]; then
    capi_sha="$(python3 "$repo_dir/scripts/desktop_native_catalog.py" verify "$native_root")"
    export VOICETYPE_SHERPA_CAPI_SHA256="$capi_sha"
fi
# Rust diagnostics embedded in the shipped executable must not expose private
# builder paths. CARGO_ENCODED_RUSTFLAGS preserves spaces as part of one flag.
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$repo_dir=/voicetype"$'\x1f'"--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"
# build.rs verifies the exact selected C API, fixed header and CPU ORT hashes.
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
python3 "$repo_dir/desktop/tests/runtime_layout.py" "$staged_dir" "$capi_sha"
if [[ $# == 3 ]]; then
    cp -R -- "$native_root/licenses" "$native_root/provenance" "$staged_dir/"
    cp -- "$native_root/build-provenance.json" "$staged_dir/"
    install -m 644 -- "$repo_dir/LICENSE" "$staged_dir/licenses/VoiceType.txt"
    python3 "$repo_dir/scripts/desktop_native_catalog.py" catalog "$staged_dir"
fi
# The source-built bundle includes native notices/catalog. System dependencies,
# input integration and explicit application activation remain separate steps.
mv -T --no-clobber -- "$staged_dir" "$bundle_dir"
if [[ -d "$staged_dir" ]]; then
    echo 'The destination appeared during the build; it has been preserved.' >&2
    exit 1
fi
echo "Prepared only: $bundle_dir"
