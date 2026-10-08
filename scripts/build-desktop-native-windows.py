#!/usr/bin/env python3
"""Build reviewed Nano CPU C API on Windows from pinned source inputs."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
RECIPE = ROOT / "desktop/assets/windows-native-sources.json"
SHARED = ROOT / "desktop/assets/linux-native-sources.json"
PATCH = ROOT / "patches/sherpa-onnx-1.13.8-nano-integrity.patch"
base = runpy.run_path(str(ROOT / "scripts/build-desktop-native.py"))
fetch, extract, copy = base["fetch"], base["extract"], base["copy"]


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def require(ok, message):
    if not ok:
        raise RuntimeError(message)


def recipe():
    win = json.loads(RECIPE.read_text())
    shared = json.loads(SHARED.read_text())
    require(win["platform"] == "windows-x86_64" and win["provider"] == "cpu", "Wrong native platform")
    require(sha(SHARED) == win["shared_source_recipe_sha256"], "Shared source pins changed")
    require(sha(PATCH) == win["patch_sha256"] == shared["patch_sha256"], "Integrity patch changed")
    require(win["source_commit"] == shared["source_commit"], "Different Sherpa source revision")
    return win, shared


def ort_files(archive, config, destination):
    prefix = "onnxruntime-win-x64-1.28.2/"
    with zipfile.ZipFile(archive) as source:
        for relative, expected in config["members"].items():
            inside = prefix + (relative if not relative.startswith("licenses/onnxruntime/")
                               else relative.removeprefix("licenses/onnxruntime/"))
            data = source.read(inside)
            require(hashlib.sha256(data).hexdigest() == expected, "ORT member mismatch: " + relative)
            output = destination / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_bytes(data)


def checked_source_inputs(shared, cache, offline, work):
    sources = work / "sources"
    sources.mkdir()
    payload = work / "payload"
    for item in shared["inputs"]:
        if item["id"] == "onnxruntime":
            continue  # Linux ELF is not a Windows build input.
        archive = fetch(item, cache, offline)
        if item["format"] == "file":
            target = item["destination"]
            copy(archive, (payload if target.startswith("licenses/") else work) / target)
        else:
            extract(archive, sources / item["id"], item["format"])
            for notice in item["licenses"]:
                copy(sources / item["id"] / notice,
                     payload / "licenses" / item["id"] / notice)
    sherpa = sources / "sherpa-onnx"
    # git apply needs a repository root; create one only inside temporary source.
    subprocess.run(["git", "-C", str(sherpa), "init", "--quiet"], check=True)
    subprocess.run(["git", "-C", str(sherpa), "apply", "--check", str(PATCH)], check=True)
    subprocess.run(["git", "-C", str(sherpa), "apply", str(PATCH)], check=True)
    for extension, expected in [
        ("cc", "6e83fb54b5c468370eafa0301fd4372ca70f0e5049fc7d0553e477ae184cf7d8"),
        ("h", "0651bce685bd79d86c4d56b553e796c38917b6dafef87318c8f0c573e6be1b96"),
    ]:
        require(sha(sherpa / f"sherpa-onnx/csrc/offline-recognizer-funasr-nano-impl.{extension}") == expected,
                "Patched Nano source changed")
    header = sherpa / "sherpa-onnx/c-api/c-api.h"
    require(sha(header) == "2a1b95084be8fd1deb3228fcad2fd3f7f0258b64582f7402281ec174c7b7f4ce",
            "C API header changed")
    copy(header, payload / "include/sherpa-onnx/c-api/c-api.h")
    return sources, sherpa, payload


def build(args):
    win, shared = recipe()
    if args.check_recipe:
        print("Windows Nano source pins and patch verified")
        return
    require(args.check_inputs or sys.platform == "win32", "Run native Windows build on Windows x64")
    require(1 <= args.jobs <= 4, "Choose 1–4 CPU jobs")
    output = args.output.absolute()
    if not args.check_inputs:
        require(not output.exists() and not output.is_symlink(), "Output exists; choose new directory")
    cache = args.cache.absolute()
    cache.mkdir(parents=True, exist_ok=True)
    ort_archive = fetch(win["onnxruntime"], cache, args.offline)
    scratch = ROOT / ".scratch/windows-native"
    scratch.mkdir(parents=True, exist_ok=True)
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="build-", dir=scratch) as temporary:
        work = Path(temporary)
        sources, sherpa, payload = checked_source_inputs(shared, cache, args.offline, work)
        ort_files(ort_archive, win["onnxruntime"], payload)
        if args.check_inputs:
            print("Windows source archives, native members, headers and integrity patch verified")
            return
        ort_lib = payload / "lib"
        include = work / "ort-include"
        require(include.joinpath("onnxruntime_c_api.h").is_file(), "Missing pinned ORT headers")
        disabled = ["GPU", "DIRECTML", "TTS", "SPEAKER_DIARIZATION", "PYTHON", "JNI", "BINARY",
                    "WEBSOCKET", "PORTAUDIO", "TESTS", "RKNN", "AXERA", "AXCL", "ASCEND_NPU",
                    "QNN", "SPACEMIT", "CHECK", "SANITIZER"]
        options = ["-DCMAKE_POLICY_VERSION_MINIMUM=3.5", "-DCMAKE_POLICY_DEFAULT_CMP0091=NEW",
                   "-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreadedDLL", "-DBUILD_SHARED_LIBS=ON",
                   "-DFETCHCONTENT_FULLY_DISCONNECTED=ON", "-DSHERPA_ONNX_ENABLE_C_API=ON",
                   "-DSHERPA_ONNX_USE_PRE_INSTALLED_ONNXRUNTIME_IF_AVAILABLE=ON",
                   "-DSHERPA_ONNX_BUILD_C_API_EXAMPLES=OFF"]
        options += [f"-DSHERPA_ONNX_ENABLE_{name}=OFF" for name in disabled]
        options += [f"-DFETCHCONTENT_SOURCE_DIR_{item['cmake']}={sources / item['id']}"
                    for item in shared["inputs"] if "cmake" in item]
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(("CUDA_", "SHERPA_", "CMAKE_"))}
        env.update(SHERPA_ONNXRUNTIME_INCLUDE_DIR=str(include), SHERPA_ONNXRUNTIME_LIB_DIR=str(ort_lib))
        build_dir = work / "build"
        subprocess.run(["cmake", "-S", str(sherpa), "-B", str(build_dir), "-A", "x64", *options],
                       check=True, env=env)
        subprocess.run(["cmake", "--build", str(build_dir), "--config", "Release",
                        "--target", "sherpa-onnx-c-api", "--parallel", str(args.jobs)], check=True, env=env)
        for filename in ["sherpa-onnx-c-api.dll", "sherpa-onnx-c-api.lib"]:
            matches = list(build_dir.rglob(filename))
            require(len(matches) == 1, "Expected exactly one native build output: " + filename)
            copy(matches[0], ort_lib / filename)
        require((ort_lib / "sherpa-onnx-c-api.dll").stat().st_size > 0, "Empty C API DLL")
        copy(RECIPE, payload / "provenance/windows-native-sources.json")
        copy(SHARED, payload / "provenance/linux-native-sources.json")
        copy(PATCH, payload / "provenance/sherpa-onnx-1.13.8-nano-integrity.patch")
        copy(ROOT / "patches/README.nano-integrity.md", payload / "provenance/README.nano-integrity.md")
        provenance = {
            "schema_version": 1, "platform": "windows-x86_64", "provider": "cpu",
            "recipe_sha256": sha(RECIPE), "shared_recipe_sha256": sha(SHARED),
            "patch_sha256": sha(PATCH), "cmake_options": options,
            "files": {str(path.relative_to(payload)).replace("\\", "/"): {"bytes": path.stat().st_size,
                       "sha256": sha(path)} for path in sorted(payload.rglob("*")) if path.is_file()},
        }
        (payload / "build-provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
        with tempfile.TemporaryDirectory(prefix=".native-stage-", dir=output.parent) as stage:
            staged = Path(stage) / "payload"
            shutil.copytree(payload, staged)
            os.rename(staged, output)
    print("Prepared Windows CPU native build: " + str(output))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--jobs", type=int, default=2)
    parser.add_argument("--check-recipe", action="store_true")
    parser.add_argument("--check-inputs", action="store_true",
                        help="verify and prepare all Windows source inputs without compiling")
    args = parser.parse_args()
    if not args.check_recipe and (not args.cache or not args.output):
        parser.error("--cache and --output are required")
    try:
        build(args)
    except (RuntimeError, OSError, subprocess.CalledProcessError, zipfile.BadZipFile) as error:
        parser.exit(1, str(error) + "\n")
