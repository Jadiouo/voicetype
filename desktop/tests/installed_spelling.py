"""Verify the installed CSC resources against the catalog used at build time."""
import hashlib
import json
from pathlib import Path
import sys


def is_pe(path):
    with path.open("rb") as stream:
        header = stream.read(64)
        if len(header) != 64 or header[:2] != b"MZ":
            return False
        offset = int.from_bytes(header[0x3c:0x40], "little")
        if offset < 64 or offset > path.stat().st_size - 4:
            return False
        stream.seek(offset)
        return stream.read(4) == b"PE\0\0"


def verify(installation, expected_path):
    installation = Path(installation).resolve(strict=True)
    expected_bytes = Path(expected_path).read_bytes()
    expected = json.loads(expected_bytes)
    candidates = list(installation.rglob("spelling/manifest.json"))
    assert len(candidates) == 1, "Expected one installed spelling catalog"
    root = candidates[0].parent
    assert candidates[0].read_bytes() == expected_bytes, "Installed catalog differs from build"
    for entry in expected["files"]:
        path = root / entry["path"]
        assert path.is_file() and not path.is_symlink(), entry["path"]
        assert path.stat().st_size == entry["bytes"], entry["path"]
        with path.open("rb") as stream:
            assert hashlib.file_digest(stream, "sha256").hexdigest() == entry["sha256"], entry["path"]
    names = {entry["path"] for entry in expected["files"]}
    assert "models/model-int8-fused.onnx" in names
    assert "models/tokenizer.json" in names
    assert "licenses/MacBERT-Apache-2.0.txt" in names
    assert not any("cuda" in name.lower() or "tensorrt" in name.lower() for name in names)
    origins = json.loads((root / "provenance/binary-origins.json").read_text())
    assert all(entry["path"] in names for entry in origins["binaries"])
    if expected["platform"].startswith("windows"):
        actual_pe = {path.relative_to(root).as_posix()
                     for path in root.rglob("*") if path.is_file() and is_pe(path)}
        origin_pe = {entry["path"] for entry in origins["binaries"]}
        assert actual_pe == origin_pe, "Installed Windows PE set differs from origin catalog"
        assert len(origin_pe) == len(origins["binaries"]), "Duplicate Windows PE origin"
        excluded = origins.get("excluded_os_binaries", [])
        assert len({entry["path"] for entry in excluded}) == len(excluded), "Duplicate Windows OS exclusion"
        for entry in excluded:
            assert entry["path"] not in names and entry["path"] not in actual_pe, entry["path"]
            assert entry["bytes"] > 0 and len(entry["sha256"]) == 64
            assert entry["provided_by"] == "Windows 11/Server 2022 OS UCRT/API-set"
        for entry in origins["binaries"]:
            member = root / entry["path"]
            assert entry.get("bytes") == member.stat().st_size, entry["path"]
            with member.open("rb") as stream:
                assert entry.get("sha256") == hashlib.file_digest(stream, "sha256").hexdigest(), entry["path"]
            if origins["complete"]:
                assert entry.get("source_artifact") and entry.get("source_sha256") and entry.get("notice"), entry["path"]
    for entry in origins["binaries"]:
        if "notice" in entry:
            assert entry["notice"] in names and (root / entry["notice"]).is_file(), entry["path"]
    print(f"PASS: {len(names)} installed CSC members, model and CPU runtime; native notices complete={origins['complete']}")


def main():
    verify(sys.argv[1], sys.argv[2])


if __name__ == "__main__":
    main()
