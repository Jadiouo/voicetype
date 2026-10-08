"""Verify the installed CSC resources against the catalog used at build time."""
import hashlib
import json
from pathlib import Path
import sys


def main():
    installation = Path(sys.argv[1]).resolve(strict=True)
    expected_bytes = Path(sys.argv[2]).read_bytes()
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
    for entry in origins["binaries"]:
        if "notice" in entry:
            assert (root / entry["notice"]).is_file(), entry["path"]
    print(f"PASS: {len(names)} installed CSC members, model and CPU runtime; native notices complete={origins['complete']}")


if __name__ == "__main__":
    main()
