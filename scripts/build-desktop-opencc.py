#!/usr/bin/env python3
"""Build pinned Windows OpenCC + Taiwan dictionaries, without installing an app."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request

REVISION = "556ed22496d650bd0b13b6c163be9814637970ae"
SOURCE_URL = f"https://codeload.github.com/BYVoid/OpenCC/tar.gz/{REVISION}"
SOURCE_SHA = "2792fc0944359c5d099bd79e08bfffce250bf3760aa0ec306e975cc58314482a"
RAPIDJSON_URL = "https://raw.githubusercontent.com/Tencent/rapidjson/v1.1.0/license.txt"
RAPIDJSON_SHA = "a140e5d46fe734a1c78f1a3c3ef207871dd75648be71fdda8e309b23ab8b1f32"


def download(url, digest, path):
    with urllib.request.urlopen(url, timeout=60) as response:
        data = response.read(16 * 1024 * 1024 + 1)
    if len(data) > 16 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != digest:
        raise ValueError("Source checksum mismatch")
    path.write_bytes(data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--stage-tests", type=Path, help="Copy DLL/dictionaries beside Rust test executables")
    args = parser.parse_args()
    if sys.platform != "win32":
        raise SystemExit("This recipe builds a native Windows DLL; Linux uses system OpenCC")
    output = args.output.absolute()
    if output.exists() or output.is_symlink():
        raise SystemExit("Destination exists; choose a new directory")
    with tempfile.TemporaryDirectory(prefix="voicetype-opencc-") as temporary:
        temporary = Path(temporary)
        archive = temporary / "source.tar.gz"
        download(SOURCE_URL, SOURCE_SHA, archive)
        with tarfile.open(archive) as source:
            source.extractall(temporary, filter="data")
        root = temporary / f"OpenCC-{REVISION}"
        build = temporary / "build"
        subprocess.run(["cmake", "-S", str(root), "-B", str(build), "-A", "x64",
                        "-DCMAKE_POLICY_VERSION_MINIMUM=3.5", "-DCMAKE_POLICY_DEFAULT_CMP0091=NEW",
                        "-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded", "-DBUILD_SHARED_LIBS=ON",
                        "-DENABLE_GTEST=OFF", "-DENABLE_BENCHMARK=OFF", "-DENABLE_DARTS=OFF",
                        "-DBUILD_PYTHON=OFF", f"-DPYTHON_EXECUTABLE={sys.executable}"], check=True)
        subprocess.run(["cmake", "--build", str(build), "--config", "Release",
                        "--target", "Dictionaries", "--parallel", "2"], check=True)
        output.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix=".opencc-stage-", dir=output.parent) as stage:
            stage = Path(stage)
            (stage / "data").mkdir()
            (stage / "licenses").mkdir()
            shutil.copyfile(build / "src/Release/opencc.dll", stage / "opencc.dll")
            for name in ["STPhrases.ocd2", "STCharacters.ocd2", "TWVariants.ocd2"]:
                shutil.copyfile(build / "data" / name, stage / "data" / name)
            shutil.copyfile(root / "data/config/s2tw.json", stage / "data/s2tw.json")
            shutil.copyfile(root / "LICENSE", stage / "licenses/OpenCC.txt")
            shutil.copyfile(root / "deps/marisa-0.2.6/COPYING.md", stage / "licenses/marisa.txt")
            download(RAPIDJSON_URL, RAPIDJSON_SHA, stage / "licenses/RapidJSON.txt")
            # Use marisa's BSD option. Darts is disabled; no CLI or test libraries
            # are distributed. Preserve the exact source notice alongside it.
            provenance = {"revision": REVISION, "source_url": SOURCE_URL, "source_sha256": SOURCE_SHA,
                          "version": "1.1.9", "marisa_license_choice": "BSD-2-Clause",
                          "rapidjson_notice_url": RAPIDJSON_URL, "rapidjson_notice_sha256": RAPIDJSON_SHA}
            (stage / "licenses/SOURCE.json").write_text(json.dumps(provenance, indent=2) + "\n", encoding="utf-8")
            files = []
            for path in sorted(stage.rglob("*")):
                if path.is_file():
                    data = path.read_bytes()
                    if not data: raise ValueError("Empty OpenCC output")
                    files.append({"path": path.relative_to(stage).as_posix(), "bytes": len(data),
                                  "sha256": hashlib.sha256(data).hexdigest()})
            (stage / "manifest.json").write_text(json.dumps({"source": provenance, "files": files}, indent=2) + "\n")
            os.rename(stage, output)  # Windows refuses replacement of an existing destination.
    if args.stage_tests:
        target = args.stage_tests.absolute()
        target.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(output / "opencc.dll", target / "opencc.dll")
        shutil.copytree(output / "data", target / "opencc", dirs_exist_ok=True)
    print(f"Prepared Windows OpenCC: {output}")


if __name__ == "__main__":
    main()
