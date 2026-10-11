"""Verify packaged VoiceType helper bytes; the official CLI is never bundled."""
import hashlib
import json
from pathlib import Path
import sys


def main():
    installation = Path(sys.argv[1]).resolve(strict=True)
    expected_bytes = Path(sys.argv[2]).read_bytes()
    expected = json.loads(expected_bytes)
    candidates = list(installation.rglob("google/manifest.json"))
    assert len(candidates) == 1, "Expected one installed Google helper catalog"
    root = candidates[0].parent
    assert candidates[0].read_bytes() == expected_bytes
    names = set()
    for entry in expected["files"]:
        names.add(entry["path"])
        member = root / entry["path"]
        assert member.is_file() and not member.is_symlink(), entry["path"]
        assert member.stat().st_size == entry["bytes"], entry["path"]
        with member.open("rb") as stream:
            assert hashlib.file_digest(stream, "sha256").hexdigest() == entry["sha256"]
    suffix = ".exe" if expected["platform"].startswith("windows") else ""
    assert {"voicetype-google-editor" + suffix, "voicetype-google-pcm" + suffix,
            "LICENSE.txt"} <= names
    assert not any(name.lower().startswith(("agy", "antigravity")) for name in names)
    print("PASS: packaged Google helpers match source-built catalog; official CLI absent")


if __name__ == "__main__":
    main()
