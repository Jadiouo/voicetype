#!/usr/bin/env python3
"""Build an isolated CPU CSC bundle with model provenance and dependency notices.

Run with the venv from config/csc-build-requirements.txt. CPU only, no microphone,
user config or system service. Build on each target OS; output must be new.
"""
import argparse
import hashlib
import importlib.metadata as metadata
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import sysconfig
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from prepare_csc import REVISION, SOURCES, digest


def run(*args):
    subprocess.run([str(a) for a in args], check=True, env={**os.environ, "CUDA_VISIBLE_DEVICES": ""})


def notices(destination):
    destination.mkdir()
    versions = {}
    # These distributions reach the frozen runtime; optional HTTP model-download
    # dependencies are excluded because models/tokenizer are always local files.
    names = ["onnxruntime", "numpy", "tokenizers", "opencc-python-reimplemented",
             "pypinyin", "flatbuffers", "packaging", "protobuf", "sympy", "mpmath",
             "pyinstaller", "pyinstaller-hooks-contrib", "altgraph", "pyyaml"]
    for name in names:
        distribution = metadata.distribution(name)
        versions[name] = distribution.version
        target = destination / name
        target.mkdir()
        found = 0
        for file in distribution.files or []:
            if any(token in file.name.lower() for token in ("license", "copying", "notice")):
                source = Path(distribution.locate_file(file))
                if source.is_file() and source.stat().st_size:
                    filename = re.sub(r"[^A-Za-z0-9_.-]", "_", file.name)
                    shutil.copyfile(source, target / f"{found:02d}-{filename}")
                    found += 1
        if not found:
            missing = {
                "tokenizers": ("https://raw.githubusercontent.com/huggingface/tokenizers/v0.22.2/LICENSE",
                               "c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4"),
                "flatbuffers": ("https://raw.githubusercontent.com/google/flatbuffers/v25.12.19/LICENSE",
                                "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30"),
            }
            if name not in missing: raise RuntimeError(f"No installed license text for {name}")
            url, expected = missing[name]
            with urllib.request.urlopen(url, timeout=60) as response: data = response.read()
            if hashlib.sha256(data).hexdigest() != expected: raise RuntimeError("license hash mismatch")
            (target / "LICENSE.txt").write_bytes(data)
    python_candidates = [Path(sys.base_prefix) / "LICENSE.txt",
                         Path(sysconfig.get_path("stdlib")) / "LICENSE.txt"]
    python_license = next((p for p in python_candidates if p.is_file()), None)
    if python_license:
        shutil.copyfile(python_license, destination / "Python.txt")
    else:
        url = f"https://raw.githubusercontent.com/python/cpython/v{platform.python_version()}/LICENSE"
        with urllib.request.urlopen(url, timeout=60) as response:
            (destination / "Python.txt").write_bytes(response.read())
    shutil.copyfile(ROOT / "LICENSE", destination / "VoiceType.txt")
    versions["Python"] = platform.python_version()
    return versions


def binary_origins(bundle):
    """Account for frozen native libraries; fail Linux builds on unknown copies."""
    internal = bundle / "_internal"
    records = []
    def notice_for(package):
        if package == "Python": return "licenses/Python.txt"
        candidates = sorted((bundle / "licenses" / package).glob("*"))
        if not candidates or not candidates[0].is_file():
            raise RuntimeError("native extension notice missing: " + package)
        return candidates[0].relative_to(bundle).as_posix()
    if sys.platform == "linux":
        # PyInstaller also copies NumPy's wheel libraries to the top level.
        # Its LICENSE.txt contains OpenBLAS, libgfortran and libquadmath terms.
        wheel = {p.name for p in (internal / "numpy.libs").glob("lib*.so*")}
        listed = subprocess.check_output(["ldconfig", "-p"], text=True)
        system = {}
        for line in listed.splitlines():
            match = re.match(r"\s*(\S+) \(.*\) => (/.+)", line)
            if match: system.setdefault(match[1], Path(match[2]))
        notice_dir = bundle / "licenses/system"
        notice_dir.mkdir(parents=True, exist_ok=True)
        for binary in sorted(internal.glob("lib*.so*")):
            relative = binary.relative_to(bundle).as_posix()
            if binary.name in wheel:
                source = internal / "numpy.libs" / binary.name
                if digest(binary) != digest(source):
                    raise RuntimeError("NumPy wheel library copy changed: " + relative)
                records.append(dict(path=relative, origin="numpy wheel", notice=notice_for("numpy")))
                continue
            source = system.get(binary.name)
            if source is None or digest(binary) != digest(source):
                raise RuntimeError("unknown copied system library: " + relative)
            owner = subprocess.check_output(["dpkg-query", "-S", str(source.resolve())], text=True)
            package_with_arch = owner.split(": ", 1)[0]
            package = package_with_arch.split(":", 1)[0]
            version = subprocess.check_output(["dpkg-query", "-W", "-f=${Version}", package_with_arch], text=True)
            copyright = Path("/usr/share/doc") / package / "copyright"
            if not copyright.is_file() or not copyright.stat().st_size:
                raise RuntimeError("system library copyright missing: " + package)
            notice = notice_dir / f"{package}.txt"
            if not notice.exists(): shutil.copyfile(copyright, notice)
            records.append(dict(path=relative, origin=package, version=version,
                                notice=notice.relative_to(bundle).as_posix(), source=str(source.resolve())))
        package_roots = {
            "numpy": "numpy", "numpy.libs": "numpy", "onnxruntime": "onnxruntime",
            "tokenizers": "tokenizers", "yaml": "pyyaml", "python3.12": "Python",
        }
        accounted = {entry["path"] for entry in records}
        for binary in sorted(internal.rglob("*.so*")):
            relative = binary.relative_to(bundle).as_posix()
            if relative in accounted: continue
            component = binary.relative_to(internal).parts[0]
            package = package_roots.get(component)
            if package is None:
                raise RuntimeError("unreviewed native extension: " + relative)
            notice = notice_for(package)
            records.append(dict(path=relative, origin=package, notice=notice))
        records.append(dict(path="voicetype-csc", origin="pyinstaller bootloader",
                            notice=notice_for("pyinstaller")))
    else:
        # Windows wheels/Python distribute native DLLs under several roots. CI
        # publishes the exact list for a separate MSVC redistributable audit.
        for binary in sorted([*bundle.rglob("*.dll"), *bundle.rglob("*.pyd")]):
            records.append(dict(path=binary.relative_to(bundle).as_posix(),
                                origin="Windows native binary; license audit pending"))
    return dict(complete=sys.platform == "linux", binaries=records)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("output", type=Path)
    ap.add_argument("--cache", required=True, type=Path)
    ap.add_argument("--source-dir", type=Path, help="Optional hash-verified author ONNX cache")
    args = ap.parse_args()
    output = args.output.resolve()
    cache = args.cache.resolve()
    if output.exists() or output.is_symlink():
        ap.error("output already exists; choose a fresh bundle directory")
    if platform.machine().lower() not in ("x86_64", "amd64") or sys.platform not in ("linux", "win32"):
        ap.error("only Linux/Windows x86-64 supported")
    for name, version in {"onnxruntime":"1.24.4", "onnx":"1.20.1", "pyinstaller":"6.22.3",
                          "numpy":"2.5.3", "tokenizers":"0.22.2", "pyyaml":"6.0.3"}.items():
        if metadata.version(name) != version:
            ap.error("build environment differs from pinned requirements: " + name)
    cache.mkdir(parents=True, exist_ok=True)
    os.environ["PYINSTALLER_CONFIG_DIR"] = str(cache / "pyinstaller")
    prepared = cache / "prepared"
    if not (prepared / "prepared.json").is_file():
        command = [sys.executable, ROOT / "scripts/prepare_csc.py", "--output-dir", prepared]
        if args.source_dir: command += ["--source-dir", args.source_dir.resolve()]
        run(*command)
    info = json.loads((prepared / "prepared.json").read_text())
    if (info["source_revision"] != REVISION or info["source_hashes"] != SOURCES
            or info["provider"] != "CPUExecutionProvider" or info["ort"] != "1.24.4"
            or info["onnx"] != "1.20.1"):
        raise RuntimeError("prepared model provenance mismatch")
    for filename, expected in SOURCES.items():
        if digest(prepared / filename) != expected: raise RuntimeError("source model mismatch")
    if digest(prepared / "model-int8-fused.onnx") != info["model_sha256"]:
        raise RuntimeError("prepared model mismatch")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="spelling-build-", dir=cache) as temporary:
        work = Path(temporary)
        run(sys.executable, "-m", "PyInstaller", "--noconfirm", "--clean", "--onedir", "--noupx",
            "--name", "voicetype-csc", "--distpath", work / "dist", "--workpath", work / "build",
            "--specpath", work, "--collect-data", "opencc", "--collect-data", "pypinyin",
            "--collect-binaries", "onnxruntime", "--exclude-module", "torch", "--exclude-module", "onnx",
            "--exclude-module", "transformers", "--exclude-module", "huggingface_hub",
            ROOT / "scripts/voicetype_csc.py")
        bundle = work / "bundle"
        # Resolve PyInstaller's same-directory library symlinks into ordinary
        # files; the app's version store never follows untrusted links.
        shutil.copytree(work / "dist/voicetype-csc", bundle, symlinks=False)
        versions = notices(bundle / "licenses")
        origins = binary_origins(bundle)
        models = bundle / "models"
        models.mkdir()
        for filename in ["model-int8-fused.onnx", "tokenizer.json"]:
            shutil.copyfile(prepared / filename, models / filename)
        provenance = bundle / "provenance"
        provenance.mkdir()
        shutil.copyfile(prepared / "prepared.json", provenance / "model.json")
        shutil.copyfile(ROOT / "scripts/prepare_csc.py", provenance / "prepare_csc.py")
        shutil.copyfile(ROOT / "config/csc-requirements.txt", provenance / "requirements.txt")
        shutil.copyfile(ROOT / "config/csc-build-requirements.txt", provenance / "build-requirements.txt")
        (provenance / "packages.json").write_text(json.dumps(versions, indent=2)+"\n")
        (provenance / "binary-origins.json").write_text(json.dumps(origins, indent=2)+"\n")
        card = f"https://huggingface.co/shibing624/macbert4csc-base-chinese/raw/{REVISION}/README.md"
        with urllib.request.urlopen(card, timeout=60) as response:
            model_card = response.read()
        if b"license: apache-2.0" not in model_card:
            raise RuntimeError("model license does not match reviewed catalog")
        (provenance / "model-card.md").write_bytes(model_card)
        with urllib.request.urlopen("https://www.apache.org/licenses/LICENSE-2.0.txt", timeout=60) as response:
            (bundle / "licenses/MacBERT-Apache-2.0.txt").write_bytes(response.read())
        files = []
        executable = "voicetype-csc.exe" if sys.platform == "win32" else "voicetype-csc"
        for path in sorted(bundle.rglob("*")):
            if path.is_file():
                relative = path.relative_to(bundle).as_posix()
                if path.stat().st_size == 0: path.unlink(); continue
                if not re.fullmatch(r"[A-Za-z0-9_./+\-]+", relative):
                    raise RuntimeError("non-portable package path: " + relative)
                files.append(dict(path=relative, bytes=path.stat().st_size,
                                  sha256=digest(path), executable=relative == executable))
        if len(files) > 256: raise RuntimeError("spelling bundle exceeds catalog member limit")
        manifest = dict(schema_version=1, id="csc-runtime", version="macbert-615e6e-cpu-1",
            platform=("windows" if sys.platform == "win32" else "linux")+"-x86_64",
            source_url="https://github.com/Jadiouo/voicetype", license="GPL-3.0-only; dependencies: see licenses",
            files=files)
        (bundle / "manifest.json").write_text(json.dumps(manifest, indent=2)+"\n")
        # Staging and final directories share the output volume for atomic move.
        with tempfile.TemporaryDirectory(prefix=".spelling-", dir=output.parent) as staged:
            destination = Path(staged) / "bundle"
            shutil.copytree(bundle, destination)
            if output.exists(): raise RuntimeError("output appeared during build")
            destination.rename(output)
    print(f"Prepared CPU spelling bundle: {len(files)} verified files")


if __name__ == "__main__": main()
