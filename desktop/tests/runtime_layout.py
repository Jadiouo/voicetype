"""Verify a prepared Linux CPU runtime bundle without starting capture.

Usage: python3 desktop/tests/runtime_layout.py BUNDLE_DIRECTORY
The build/install seam is the actual ELF bundle, not build command success.
"""
import hashlib
import os
from pathlib import Path
import re
import subprocess
import sys


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def verify(root):
    root = root.resolve(strict=True)
    pins = {
        "libsherpa-onnx-c-api.so": "72408cc5f2407eb0ba46cd381614229107f225b8ccc4149e2f5e4b09957834dd",
        "libonnxruntime.so": "4b3607aebd1784b26b6f9b20e4bd974c7ab8287043e4d095cb7d2cb40b5e566e",
    }
    paths = {"bin/voicetyped": "$ORIGIN/../lib"}
    paths.update({"lib/" + name: "$ORIGIN" for name in pins})
    for relative, expected_rpath in paths.items():
        path = root / relative
        require(path.is_file() and not path.is_symlink(), "Runtime member must be a regular file")
        elf = subprocess.check_output(["readelf", "-d", str(path)], text=True)
        rpath = re.findall(r"\((?:RPATH|RUNPATH)\).*?\[(.*?)\]", elf)
        require(rpath == [expected_rpath], f"Unexpected runtime search path in {relative}: {rpath}")
        if path.name in pins:
            require(hashlib.sha256(path.read_bytes()).hexdigest() == pins[path.name], "Native library pin mismatch")
    # The loader lists dependencies without executing the daemon's main().
    elf = subprocess.check_output(["readelf", "-l", str(root / "bin/voicetyped")], text=True)
    interpreter = re.search(r"Requesting program interpreter: (.*?)\]", elf)
    require(interpreter is not None, "Missing ELF interpreter")
    loader = interpreter.group(1)
    env = {name: value for name, value in os.environ.items() if not name.startswith("LD_")}
    resolved = subprocess.check_output([loader, "--list", str(root / "bin/voicetyped")], env=env, text=True)
    for name in pins:
        match = re.search(re.escape(name) + r" => (.*?) \(", resolved)
        require(match and Path(match.group(1)).resolve() == root / "lib" / name, "Native library resolved outside bundle")
    require(not re.search(r"lib(cuda|cudnn|nvinfer)", resolved), "Unexpected GPU dependency")
    print("PASS: exact CPU library pins and bundle-relative loader paths")


if __name__ == "__main__":
    verify(Path(sys.argv[1]))
