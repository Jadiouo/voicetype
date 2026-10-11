"""Check installed resource bytes against the build-time catalog, then the loader.

Usage: installed_runtime.py EXTRACTED_INSTALLER_DIRECTORY BUILD_TIME_MANIFEST
No runtime entry point, capture or model is executed.
"""
import hashlib
import json
from pathlib import Path
import sys
from runtime_layout import require, verify


def main():
    installation = Path(sys.argv[1]).resolve(strict=True)
    expected_bytes = Path(sys.argv[2]).read_bytes()
    expected = json.loads(expected_bytes)
    candidates = list(installation.rglob("runtime/manifest.json"))
    require(len(candidates) == 1, "Expected one installed runtime catalog")
    root = candidates[0].parent
    require(candidates[0].read_bytes() == expected_bytes, "Installed catalog differs from app build")
    for entry in expected["files"]:
        path = root / entry["path"]
        require(path.is_file() and not path.is_symlink(), "Missing installed runtime member")
        require(path.stat().st_size == entry["bytes"], "Installed member size mismatch")
        require(hashlib.sha256(path.read_bytes()).hexdigest() == entry["sha256"], "Installed member SHA256 mismatch")
    capi = next(e["sha256"] for e in expected["files"] if e["path"] == "lib/libsherpa-onnx-c-api.so")
    verify(root, capi)
    require((root / "licenses/onnxruntime/ThirdPartyNotices.txt").is_file(), "Missing native notices")
    print("PASS: installed CPU runtime, notices and all catalog bytes")


if __name__ == "__main__":
    main()
