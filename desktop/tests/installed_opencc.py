"""Verify the actual installed Windows DLL/dictionaries, including Unicode paths."""
import ctypes
import hashlib
import json
from pathlib import Path
import sys

root = Path(sys.argv[1]).resolve(strict=True)
expected_bytes = Path(sys.argv[2]).read_bytes()
assert (root / "opencc-manifest.json").read_bytes() == expected_bytes
for entry in json.loads(expected_bytes)["files"]:
    relative = entry["path"]
    if relative.startswith("data/"):
        relative = "opencc/" + relative.removeprefix("data/")
    elif relative.startswith("licenses/"):
        relative = "licenses/opencc/" + relative.removeprefix("licenses/")
    path = root / relative
    assert path.is_file() and not path.is_symlink(), relative
    data = path.read_bytes()
    assert len(data) == entry["bytes"], relative
    assert hashlib.sha256(data).hexdigest() == entry["sha256"], relative

api = ctypes.CDLL(str(root / "opencc.dll"))
api.opencc_open_w.argtypes = [ctypes.c_wchar_p]
api.opencc_open_w.restype = ctypes.c_void_p
api.opencc_close.argtypes = [ctypes.c_void_p]
api.opencc_close.restype = ctypes.c_int
api.opencc_convert_utf8.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_size_t]
api.opencc_convert_utf8.restype = ctypes.c_void_p
api.opencc_convert_utf8_free.argtypes = [ctypes.c_void_p]
api.opencc_convert_utf8_free.restype = None
handle = api.opencc_open_w(str(root / "opencc/s2tw.json"))
assert handle and handle != ctypes.c_void_p(-1).value, "Installed s2tw failed to open"
try:
    for source, expected in [("软件通过测试，GitHub commit push。", "軟件通過測試，GitHub commit push。"),
                             ("台積電", "臺積電"), ("头发", "頭髮")]:
        source = source.encode("utf-8")
        pointer = api.opencc_convert_utf8(handle, source, len(source))
        assert pointer, "Conversion failed"
        try:
            assert ctypes.string_at(pointer).decode("utf-8") == expected
        finally:
            api.opencc_convert_utf8_free(pointer)
finally:
    api.opencc_close(handle)
print("PASS: installed OpenCC bytes, notices, native s2tw and mixed English conversion")
