#!/usr/bin/env python3
"""Build the Linux CPU C API from reviewed inputs, without installed runtimes.

The output is a build input, not a downloaded trust anchor. Release builders must
run this script from reviewed source and embed the resulting payload hashes in
the app. A provenance JSON placed beside an arbitrary binary is not authentication.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import stat
import subprocess
import tarfile
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
RECIPE = ROOT / "desktop/assets/linux-native-sources.json"
PATCH = ROOT / "patches/sherpa-onnx-1.13.8-nano-integrity.patch"
HEADER_SHA = "2a1b95084be8fd1deb3228fcad2fd3f7f0258b64582f7402281ec174c7b7f4ce"
ORT_SHA = "4b3607aebd1784b26b6f9b20e4bd974c7ab8287043e4d095cb7d2cb40b5e566e"


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def fetch(entry, cache, offline):
    path = cache / (entry["id"] + ".archive")
    if not path.exists():
        require(not offline, "Missing offline input: " + entry["id"])
        with tempfile.TemporaryDirectory(prefix=".download-", dir=cache) as tmp:
            partial = Path(tmp) / "download"
            subprocess.run([
                "curl", "--disable", "--fail", "--location", "--silent", "--show-error",
                "--proto", "=https", "--proto-redir", "=https", "--max-redirs", "5",
                "--connect-timeout", "15", "--max-time", "300",
                "--max-filesize", str(entry["bytes"]), "--output", str(partial), entry["url"],
            ], check=True)
            require(partial.stat().st_size == entry["bytes"] and sha(partial) == entry["sha256"],
                    "SHA256/size mismatch: " + entry["id"])
            # Do not overwrite another builder's cache entry.
            try:
                os.link(partial, path)
            except FileExistsError:
                pass
    require(path.is_file() and not path.is_symlink()
            and path.stat().st_size == entry["bytes"] and sha(path) == entry["sha256"],
            "SHA256/size mismatch: " + entry["id"])
    return path


def extract(archive, destination, kind):
    """Only ordinary files/directories; never follow source-archive links.

    Sherpa includes unrelated absolute example symlinks. They are intentionally
    omitted, as are other special entries. The exact reviewed archives are hashed
    before this function and limited again while extracting.
    """
    destination.mkdir()
    seen = set()
    prefix = None
    total = 0

    def target(name, size):
        nonlocal prefix, total
        parts = PurePosixPath(name).parts
        require(parts and not name.startswith("/") and ".." not in parts and "\\" not in name,
                "Unsafe source path")
        prefix = prefix or parts[0]
        require(parts[0] == prefix, "Multiple source roots")
        total += size
        require(total <= 512 * 1024 * 1024 and len(seen) < 50000, "Source extraction limit")
        relative = Path(*parts[1:])
        require(relative not in seen, "Duplicate source member")
        seen.add(relative)
        return destination / relative

    if kind == "tar":
        with tarfile.open(archive) as source:
            for member in source:
                if not member.isfile() and not member.isdir():
                    continue
                path = target(member.name, member.size)
                if member.isdir():
                    path.mkdir(parents=True, exist_ok=True)
                else:
                    path.parent.mkdir(parents=True, exist_ok=True)
                    with source.extractfile(member) as src, path.open("xb") as dst:
                        shutil.copyfileobj(src, dst, 64 * 1024)
    else:
        with zipfile.ZipFile(archive) as source:
            for member in source.infolist():
                mode = member.external_attr >> 16
                if stat.S_ISLNK(mode):
                    continue
                path = target(member.filename, member.file_size)
                if member.is_dir():
                    path.mkdir(parents=True, exist_ok=True)
                else:
                    path.parent.mkdir(parents=True, exist_ok=True)
                    with source.open(member) as src, path.open("xb") as dst:
                        shutil.copyfileobj(src, dst, 64 * 1024)


def copy(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)


def build(args):
    require(platform.system() == "Linux" and platform.machine() == "x86_64", "Linux x86_64 required")
    require(1 <= args.jobs <= 4, "Use between one and four CPU build jobs")
    # Do not resolve away a destination symlink before checking it.
    output = args.output.absolute()
    require(not output.exists() and not output.is_symlink(), "Output already exists; choose a new directory")
    recipe = json.loads(RECIPE.read_text())
    require(sha(PATCH) == recipe["patch_sha256"], "Integrity patch hash mismatch")
    args.cache.mkdir(parents=True, exist_ok=True)
    inputs = [(entry, fetch(entry, args.cache, args.offline)) for entry in recipe["inputs"]]
    output.parent.mkdir(parents=True, exist_ok=True)
    # Upstream CMake's version-script linker option is not quoted. Keep its
    # entire source/build tree in a private, space-free Linux scratch directory;
    # the caller's cache and output paths may still contain spaces.
    with tempfile.TemporaryDirectory(prefix="voicetype-native-build-", dir="/tmp") as tmp:
        work = Path(tmp)
        payload = work / "payload"
        sources = work / "sources"
        sources.mkdir()
        for entry, archive in inputs:
            if entry["format"] == "file":
                destination = entry["destination"]
                copy(archive, (payload if destination.startswith("licenses/") else work) / destination)
            else:
                extract(archive, sources / entry["id"], entry["format"])
                for notice in entry["licenses"]:
                    copy(sources / entry["id"] / notice, payload / "licenses" / entry["id"] / notice)
        sherpa = sources / "sherpa-onnx"
        subprocess.run(["patch", "--batch", "--fuzz=0", "--directory", str(sherpa), "--strip=1",
                        "--input", str(PATCH)], check=True)
        for extension, expected in [
            ("cc", "6e83fb54b5c468370eafa0301fd4372ca70f0e5049fc7d0553e477ae184cf7d8"),
            ("h", "0651bce685bd79d86c4d56b553e796c38917b6dafef87318c8f0c573e6be1b96"),
        ]:
            require(sha(sherpa / f"sherpa-onnx/csrc/offline-recognizer-funasr-nano-impl.{extension}") == expected,
                    "Patched source hash mismatch")
        ort = sources / "onnxruntime/lib/libonnxruntime.so"
        require(sha(ort) == ORT_SHA, "ONNX Runtime library hash mismatch")
        header = sherpa / "sherpa-onnx/c-api/c-api.h"
        require(sha(header) == HEADER_SHA, "C API header hash mismatch")
        disabled = ["GPU", "DIRECTML", "TTS", "SPEAKER_DIARIZATION", "PYTHON", "JNI", "BINARY",
                    "WEBSOCKET", "PORTAUDIO", "TESTS", "RKNN", "AXERA", "AXCL", "ASCEND_NPU",
                    "QNN", "SPACEMIT", "CHECK", "SANITIZER"]
        cmake_options = ["-DCMAKE_BUILD_TYPE=Release", "-DBUILD_SHARED_LIBS=ON",
                         "-DCMAKE_SHARED_LINKER_FLAGS=-Wl,--disable-new-dtags",
                         "-DFETCHCONTENT_FULLY_DISCONNECTED=ON", "-DSHERPA_ONNX_ENABLE_C_API=ON",
                         "-DSHERPA_ONNX_USE_PRE_INSTALLED_ONNXRUNTIME_IF_AVAILABLE=ON",
                         "-DSHERPA_ONNX_BUILD_C_API_EXAMPLES=OFF"]
        cmake_options += [f"-DSHERPA_ONNX_ENABLE_{name}=OFF" for name in disabled]
        env = {k: v for k, v in os.environ.items()
               if not k.startswith(("LD_", "CMAKE_", "SHERPA_"))
               and k not in {"CFLAGS", "CXXFLAGS", "CPPFLAGS", "LDFLAGS", "CC", "CXX"}}
        env.update(SHERPA_ONNXRUNTIME_INCLUDE_DIR=str(work / "ort-include"),
                   SHERPA_ONNXRUNTIME_LIB_DIR=str(ort.parent))
        # Source filenames in diagnostics must not embed the builder's home.
        flags = f"-ffile-prefix-map={work}=/voicetype-native-src"
        build_dir = work / "build"
        configure = ["cmake", "-S", str(sherpa), "-B", str(build_dir), "-G", "Ninja",
                     f"-DCMAKE_C_FLAGS={flags}", f"-DCMAKE_CXX_FLAGS={flags}"] + cmake_options
        configure += [f"-DFETCHCONTENT_SOURCE_DIR_{e['cmake']}={sources / e['id']}"
                      for e, _ in inputs if "cmake" in e]
        subprocess.run(configure, env=env, check=True)
        cache = (build_dir / "CMakeCache.txt").read_text()
        enabled = re.findall(r"^SHERPA_ONNX_ENABLE_(\w+):BOOL=ON$", cache, re.M)
        require(enabled == ["C_API"], "Unexpected enabled native capability: " + repr(enabled))
        subprocess.run(["cmake", "--build", str(build_dir), "--target", "sherpa-onnx-c-api",
                        "--parallel", str(args.jobs)], env=env, check=True)
        lib = payload / "lib/libsherpa-onnx-c-api.so"
        copy(build_dir / "lib/libsherpa-onnx-c-api.so", lib)
        elf = subprocess.check_output(["readelf", "-d", str(lib)], text=True)
        rpaths = re.findall(r"\((?:RPATH|RUNPATH)\).*?\[(.*?)\]", elf)
        require(len(rpaths) == 1 and '(RPATH)' in elf, "Missing C API RPATH")
        # CMake changes the existing ELF string in place; no patchelf dependency.
        rpath_script = work / "rpath.cmake"
        rpath_script.write_text('file(RPATH_CHANGE FILE [=[' + str(lib) + ']=] OLD_RPATH [=['
                               + rpaths[0] + ']=] NEW_RPATH "$ORIGIN")\n')
        subprocess.run(["cmake", "-P", str(rpath_script)], check=True)
        copy(ort, payload / "lib/libonnxruntime.so")
        copy(header, payload / "include/sherpa-onnx/c-api/c-api.h")
        copy(RECIPE, payload / "provenance/linux-native-sources.json")
        copy(PATCH, payload / "provenance/sherpa-onnx-1.13.8-nano-integrity.patch")
        copy(ROOT / "patches/README.nano-integrity.md", payload / "provenance/README.nano-integrity.md")
        provenance = {
            "schema_version": 1, "platform": "linux-x86_64", "provider": "cpu",
            "recipe_sha256": sha(RECIPE), "patch_sha256": sha(PATCH),
            "compiler": subprocess.check_output(["c++", "--version"], env=env, text=True).splitlines()[0],
            "cmake": subprocess.check_output(["cmake", "--version"], text=True).splitlines()[0],
            "cmake_options": cmake_options,
            "files": {str(p.relative_to(payload)): {"bytes": p.stat().st_size, "sha256": sha(p)}
                      for p in sorted(payload.rglob("*")) if p.is_file()},
        }
        (payload / "build-provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
        # Stage on the destination filesystem before publishing. /tmp may be a
        # different volume, so moving directly from scratch could expose a
        # partially copied output. Never replace even an empty existing output.
        with tempfile.TemporaryDirectory(prefix=".native-stage-", dir=output.parent) as staged:
            ready = Path(staged) / "payload"
            shutil.copytree(payload, ready)
            subprocess.run(["mv", "-T", "--no-clobber", "--", str(ready), str(output)], check=True)
            require(not ready.exists(), "Output appeared during the build; preserved existing directory")
    print("Prepared CPU native build: " + str(output))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--jobs", type=int, default=2)
    try:
        build(parser.parse_args())
    except (RuntimeError, OSError, subprocess.CalledProcessError, tarfile.TarError, zipfile.BadZipFile) as error:
        parser.exit(1, str(error) + "\n")
