"""Verify installed target-specific Rust notices and native catalog references."""
import hashlib
import json
from pathlib import Path
import sys


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def verify(installation, expected_catalog):
    expected_bytes = expected_catalog.read_bytes()
    expected = json.loads(expected_bytes)
    assert expected["complete"] is True
    candidates = list(installation.rglob("licenses/rust/manifest.json"))
    assert len(candidates) == 1, "Expected one installed Rust notice catalog"
    root = candidates[0].parent
    assert candidates[0].read_bytes() == expected_bytes
    listed = {item["path"] for item in expected["files"]}
    actual = {path.relative_to(root).as_posix() for path in root.rglob("*")
              if path.is_file() and path != candidates[0]}
    assert listed == actual, "Installed Rust notice member set differs from catalog"
    for item in expected["files"]:
        path = root / item["path"]
        assert not path.is_symlink() and path.stat().st_size == item["bytes"]
        assert digest(path) == item["sha256"], item["path"]
    packages = expected["packages"]
    assert len({(p["name"], p["version"], p["source"]) for p in packages}) == len(packages)
    for package in packages:
        assert package["license_expression"] and package["selected_license"]
        assert package["scopes"] and package["notices"]
        assert set(package["scopes"]) == set(package["scope_features"])
        assert all(notice["path"] in listed for notice in package["notices"])
        if package["source"] != "workspace":
            assert package["crate_checksum"] and len(package["crate_checksum"]) == 64
    for component in expected["native_components"]:
        name = component["name"]
        filename = "opencc-manifest.json" if name == "opencc" else f"{name}/manifest.json"
        matches = list(installation.rglob(filename))
        assert len(matches) == 1, f"Expected installed {name} native catalog"
        assert digest(matches[0]) == component["catalog_sha256"], name
        assert component["notice_count"] > 0, name
    return len(packages), len(listed), len(expected["native_components"])


if __name__ == "__main__":
    result = verify(Path(sys.argv[1]).resolve(strict=True), Path(sys.argv[2]).resolve(strict=True))
    print(f"PASS: installed Rust notices: {result[0]} locked packages, {result[1]} texts, {result[2]} native catalogs")
