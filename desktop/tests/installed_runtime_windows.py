"""Inspect actual installed Windows runtime bytes and load CPU native DLLs."""
import ctypes
import hashlib
import json
import os
from pathlib import Path
import sys

root = Path(sys.argv[1]).resolve(strict=True)
catalog = Path(sys.argv[2]).read_bytes()
runtime = root / "runtime"
assert (runtime / "manifest.json").read_bytes() == catalog
manifest = json.loads(catalog)
assert manifest["platform"] == "windows-x86_64"
assert manifest["id"] == "nano-runtime"
for entry in manifest["files"]:
    relative = Path(entry["path"])
    assert not relative.is_absolute() and ".." not in relative.parts
    path = runtime / relative
    assert path.is_file() and not path.is_symlink(), relative
    assert path.stat().st_size == entry["bytes"], relative
    assert hashlib.sha256(path.read_bytes()).hexdigest() == entry["sha256"], relative

bin_dir = runtime / "bin"
assert (bin_dir / "voicetyped.exe").read_bytes()[:2] == b"MZ"
assert (bin_dir / "sherpa-onnx-c-api.dll").read_bytes()[:2] == b"MZ"
assert (bin_dir / "onnxruntime.dll").read_bytes()[:2] == b"MZ"
with os.add_dll_directory(str(bin_dir)):
    api = ctypes.WinDLL(str(bin_dir / "sherpa-onnx-c-api.dll"))
    api.SherpaOnnxGetVersionStr.restype = ctypes.c_char_p
    api.SherpaOnnxGetOnnxruntimeVersionStr.restype = ctypes.c_char_p
    assert api.SherpaOnnxGetVersionStr() == b"1.13.8"
    assert api.SherpaOnnxGetOnnxruntimeVersionStr() == b"1.28.2"
print("PASS: installed Windows CPU runtime catalog, PE files, native DLL load and versions")
