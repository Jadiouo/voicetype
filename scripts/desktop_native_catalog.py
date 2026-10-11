"""Check trusted build outputs and generate the app's compile-time catalog.

These are build tools, not runtime manifest discovery. Trust originates from the
reviewed CI source build; the app embeds the resulting catalog at compile time.
"""
import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def verify(native):
    recipe = ROOT / "desktop/assets/linux-native-sources.json"
    metadata = json.loads((native / "build-provenance.json").read_text())
    require(metadata["recipe_sha256"] == sha(recipe), "Native recipe differs from reviewed source")
    require(metadata["patch_sha256"] == json.loads(recipe.read_text())["patch_sha256"], "Native patch mismatch")
    require(metadata["platform"] == "linux-x86_64" and metadata["provider"] == "cpu", "Unsupported native build")
    for relative, entry in metadata["files"].items():
        require(relative and not relative.startswith("/") and ".." not in Path(relative).parts,
                "Invalid build member path")
        path = native / relative
        require(path.is_file() and not path.is_symlink() and path.stat().st_size == entry["bytes"]
                and sha(path) == entry["sha256"], "Native build member mismatch: " + relative)
    require(sha(native / "provenance/linux-native-sources.json") == sha(recipe), "Bundled source recipe mismatch")
    return sha(native / "lib/libsherpa-onnx-c-api.so")


def catalog(bundle):
    files = []
    for path in sorted(bundle.rglob("*")):
        require(not path.is_symlink(), "Runtime cannot contain symlinks")
        if path.is_file() and path.name != "manifest.json":
            files.append({"path": path.relative_to(bundle).as_posix(), "bytes": path.stat().st_size,
                          "sha256": sha(path), "executable": path.relative_to(bundle).as_posix() == "bin/voicetyped"})
    manifest = {"schema_version": 1, "id": "nano-runtime", "version": "1.13.8-integrity-1",
                "platform": "linux-x86_64", "source_url": "https://github.com/Jadiouo/voicetype",
                "license": "GPL-3.0-only; bundled dependencies: see licenses", "files": files}
    (bundle / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")


if __name__ == "__main__":
    if sys.argv[1] == "verify":
        print(verify(Path(sys.argv[2])))
    elif sys.argv[1] == "catalog":
        catalog(Path(sys.argv[2]))
    else:
        raise SystemExit("Expected verify or catalog")
