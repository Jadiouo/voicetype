#!/usr/bin/env bash
# Contract tests use a fake C API and the real shim; no models or inference.
set -euo pipefail
repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
nano_native_root="${VOICETYPE_SHERPA_NATIVE_ROOT:-$repo_dir/private/runtime/sherpa-nano-integrity-1.13.8}"
nano_test_dir="$repo_dir/private/validation/nano-shim-contract"
mkdir -p -- "$nano_test_dir"
exec 9>/tmp/voicetype-evaluation.lock
flock 9
ulimit -c 0
"${CXX:-c++}" -std=c++17 -O1 -g -fsanitize=address,undefined \
    -fno-omit-frame-pointer \
    -I "$repo_dir/voicetyped/shim" -I "$nano_native_root/include" \
    "$repo_dir/voicetyped/shim/nano_shim.cpp" \
    "$repo_dir/voicetyped/shim/tests/nano_integrity_test.cpp" \
    -o "$nano_test_dir/contract-test"
ASAN_OPTIONS=detect_leaks=1 "$nano_test_dir/contract-test"
