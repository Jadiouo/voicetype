#!/usr/bin/env python3
"""Build and catalog the app-owned Windows Nano stdio process and CPU DLLs."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
WIN_RECIPE = ROOT / "desktop/assets/windows-native-sources.json"
SHARED_RECIPE = ROOT / "desktop/assets/linux-native-sources.json"


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def require(ok, message):
    if not ok:
        raise RuntimeError(message)


def verify_catalog(root):
    catalog = json.loads((root / "manifest.json").read_text())
    for item in catalog["files"]:
        relative = Path(item["path"])
        require(not relative.is_absolute() and ".." not in relative.parts, "Unsafe catalog path")
        path = root / relative
        require(path.is_file() and not path.is_symlink() and path.stat().st_size == item["bytes"]
                and sha(path) == item["sha256"], "Catalog member mismatch: " + item["path"])
    return catalog


def verify_native(native):
    recipe = json.loads(WIN_RECIPE.read_text())
    metadata = json.loads((native / "build-provenance.json").read_text())
    require(metadata["platform"] == "windows-x86_64" and metadata["provider"] == "cpu", "Wrong native build")
    require(metadata["recipe_sha256"] == sha(WIN_RECIPE)
            and metadata["shared_recipe_sha256"] == sha(SHARED_RECIPE)
            and metadata["patch_sha256"] == recipe["patch_sha256"], "Native source provenance mismatch")
    for relative, entry in metadata["files"].items():
        path = native / relative
        require(path.is_file() and not path.is_symlink() and path.stat().st_size == entry["bytes"]
                and sha(path) == entry["sha256"], "Native file mismatch: " + relative)
    require(sha(native / "provenance/windows-native-sources.json") == sha(WIN_RECIPE), "Bundled recipe mismatch")
    return sha(native / "lib/sherpa-onnx-c-api.dll")


def catalog(bundle):
    files = []
    for path in sorted(bundle.rglob("*")):
        require(not path.is_symlink(), "No runtime links allowed")
        if path.is_file() and path.name != "manifest.json":
            relative = path.relative_to(bundle).as_posix()
            require(path.stat().st_size > 0, "Empty runtime member")
            files.append({"path": relative, "bytes": path.stat().st_size,
                          "sha256": sha(path), "executable": relative == "bin/voicetyped.exe"})
    require(len(files) <= 256, "Runtime catalog too large")
    manifest = {"schema_version": 1, "id": "nano-runtime", "version": "1.13.8-integrity-1",
                "platform": "windows-x86_64", "source_url": "https://github.com/Jadiouo/voicetype",
                "license": "GPL-3.0-only; bundled dependencies: see licenses", "files": files}
    (bundle / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    verify_catalog(bundle)


def main(args):
    require(sys.platform == "win32", "Run Windows runtime build on Windows x64")
    output = args.output.absolute()
    require(not output.exists() and not output.is_symlink(), "Output exists; choose new directory")
    native = args.native.absolute()
    opencc = args.opencc.absolute()
    capi_sha = verify_native(native)
    verify_catalog(opencc)
    scratch = ROOT / ".scratch/windows-runtime"
    scratch.mkdir(parents=True, exist_ok=True)
    output.parent.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env["VOICETYPE_SHERPA_NATIVE_ROOT"] = str(native)
    env["VOICETYPE_SHERPA_CAPI_SHA256"] = capi_sha
    env["CARGO_TARGET_DIR"] = str(scratch / "cargo-target")
    env["CARGO_ENCODED_RUSTFLAGS"] = "--remap-path-prefix=" + str(ROOT) + "=/voicetype"
    subprocess.run(["cargo", "build", "--manifest-path", str(ROOT / "voicetyped/Cargo.toml"),
                    "--locked", "--release", "--no-default-features", "--features", "relocatable-runtime",
                    "-j", "2"], check=True, env=env)
    executable = scratch / "cargo-target/release/voicetyped.exe"
    require(executable.is_file() and executable.stat().st_size > 0, "Windows daemon missing")
    with tempfile.TemporaryDirectory(prefix=".runtime-stage-", dir=output.parent) as temp:
        staged = Path(temp) / "payload"
        (staged / "bin").mkdir(parents=True)
        (staged / "licenses").mkdir()
        shutil.copyfile(executable, staged / "bin/voicetyped.exe")
        for name in ["sherpa-onnx-c-api.dll", "onnxruntime.dll", "onnxruntime_providers_shared.dll"]:
            shutil.copyfile(native / "lib" / name, staged / "bin" / name)
        shutil.copyfile(opencc / "opencc.dll", staged / "bin/opencc.dll")
        shutil.copytree(opencc / "data", staged / "bin/opencc")
        shutil.copytree(native / "licenses", staged / "licenses/native")
        shutil.copytree(opencc / "licenses", staged / "licenses/opencc")
        shutil.copyfile(ROOT / "LICENSE", staged / "licenses/VoiceType.txt")
        shutil.copytree(native / "provenance", staged / "provenance")
        shutil.copyfile(native / "build-provenance.json", staged / "build-provenance.json")
        catalog(staged)
        os.rename(staged, output)
    print("Prepared Windows Nano runtime: " + str(output))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native", type=Path, required=True)
    parser.add_argument("--opencc", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    try:
        main(parser.parse_args())
    except (RuntimeError, OSError, subprocess.CalledProcessError, KeyError) as error:
        parser.exit(1, str(error) + "\n")
