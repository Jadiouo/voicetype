"""Inspect bundled Fcitx bytes, loader dependencies and native Caps restoration."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys

def main():
    installation = Path(sys.argv[1]).resolve(strict=True)
    expected = Path(sys.argv[2]).read_bytes()
    candidates = list(installation.rglob("input/manifest.json"))
    assert len(candidates) == 1, "Expected one input module catalog"
    manifest = candidates[0]
    assert manifest.read_bytes() == expected
    for entry in json.loads(expected)["files"]:
        member = manifest.parent / entry["path"]
        assert member.is_file() and not member.is_symlink()
        content = member.read_bytes()
        assert len(content) == entry["bytes"]
        assert hashlib.sha256(content).hexdigest() == entry["sha256"]
    module = manifest.parent / "libvoicetype.so"
    symbols = subprocess.check_output(["nm", "-D", str(module)], text=True)
    assert "xcb_xkb_latch_lock_state" in symbols, "Release module lost Caps Lock restoration"
    assert "fcitx_addon_factory_instance" in symbols, "Missing Fcitx entry point"
    loader = subprocess.check_output(["ldd", str(module)], text=True)
    assert "not found" not in loader, loader
    assert "desktopInputRoute" in symbols, "Missing app routing adapter"
    print("PASS: packaged Fcitx module, catalog bytes, loader and Caps Lock support")

if __name__ == "__main__":
    main()
