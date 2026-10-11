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

# Exact members observed in the 7e1944f Windows installer. Windows 11/Server
# 2022 use their system UCRT and API-set forwarders; Microsoft documents that
# app-local UCRT is ignored on Windows 10/11. Do not extend this allowlist to
# MSVC runtime libraries or to newly observed DLLs without a fresh review.
# https://learn.microsoft.com/en-us/cpp/windows/universal-crt-deployment
WINDOWS_OS_UCRT_DLLS = frozenset({
    "api-ms-win-core-console-l1-1-0.dll",
    "api-ms-win-core-datetime-l1-1-0.dll",
    "api-ms-win-core-debug-l1-1-0.dll",
    "api-ms-win-core-errorhandling-l1-1-0.dll",
    "api-ms-win-core-fibers-l1-1-0.dll",
    "api-ms-win-core-fibers-l1-1-1.dll",
    "api-ms-win-core-file-l1-1-0.dll",
    "api-ms-win-core-file-l1-2-0.dll",
    "api-ms-win-core-file-l2-1-0.dll",
    "api-ms-win-core-handle-l1-1-0.dll",
    "api-ms-win-core-heap-l1-1-0.dll",
    "api-ms-win-core-interlocked-l1-1-0.dll",
    "api-ms-win-core-kernel32-legacy-l1-1-1.dll",
    "api-ms-win-core-libraryloader-l1-1-0.dll",
    "api-ms-win-core-localization-l1-2-0.dll",
    "api-ms-win-core-memory-l1-1-0.dll",
    "api-ms-win-core-namedpipe-l1-1-0.dll",
    "api-ms-win-core-processenvironment-l1-1-0.dll",
    "api-ms-win-core-processthreads-l1-1-0.dll",
    "api-ms-win-core-processthreads-l1-1-1.dll",
    "api-ms-win-core-profile-l1-1-0.dll",
    "api-ms-win-core-rtlsupport-l1-1-0.dll",
    "api-ms-win-core-string-l1-1-0.dll",
    "api-ms-win-core-synch-l1-1-0.dll",
    "api-ms-win-core-synch-l1-2-0.dll",
    "api-ms-win-core-sysinfo-l1-1-0.dll",
    "api-ms-win-core-sysinfo-l1-2-0.dll",
    "api-ms-win-core-timezone-l1-1-0.dll",
    "api-ms-win-core-util-l1-1-0.dll",
    "api-ms-win-crt-conio-l1-1-0.dll",
    "api-ms-win-crt-convert-l1-1-0.dll",
    "api-ms-win-crt-environment-l1-1-0.dll",
    "api-ms-win-crt-filesystem-l1-1-0.dll",
    "api-ms-win-crt-heap-l1-1-0.dll",
    "api-ms-win-crt-locale-l1-1-0.dll",
    "api-ms-win-crt-math-l1-1-0.dll",
    "api-ms-win-crt-private-l1-1-0.dll",
    "api-ms-win-crt-process-l1-1-0.dll",
    "api-ms-win-crt-runtime-l1-1-0.dll",
    "api-ms-win-crt-stdio-l1-1-0.dll",
    "api-ms-win-crt-string-l1-1-0.dll",
    "api-ms-win-crt-time-l1-1-0.dll",
    "api-ms-win-crt-utility-l1-1-0.dll",
    "ucrtbase.dll",
})

PYINSTALLER_WINDOWS_WHEEL_URL = (
    "https://files.pythonhosted.org/packages/24/83/"
    "c5422204772a27aef643280fdc887b7ef55679989096b21dceb1b9625fc4/"
    "pyinstaller-6.22.3-py3-none-win_amd64.whl"
)
PYINSTALLER_WINDOWS_WHEEL_SHA256 = "500bd58c7bf7e584a8435adccbd763a0b918d5c12b08d74ff50fd79b2915458b"
PYINSTALLER_WINDOWS_BOOTLOADER_MEMBER = "PyInstaller/bootloader/Windows-64bit-intel/run.exe"
PYINSTALLER_WINDOWS_BOOTLOADER_SHA256 = "581ea23eb35cee8f9df8835c892e91416df95ca629b1a6050289aea19390e08d"
WINDOWS_PE_SOURCES = ROOT / "config/csc-windows-pe-sources.json"
WINDOWS_MSVC_LICENSE = ROOT / "config/csc-windows-msvc-license-14.51.36231.rtf"


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


def python_runtime_source(binary, prefix=None):
    """Match a copied libpython against this interpreter, not the OS default."""
    runtime_names = {sysconfig.get_config_var("LDLIBRARY"),
                     sysconfig.get_config_var("INSTSONAME")}
    if binary.name not in runtime_names:
        return None
    base = Path(prefix) if prefix is not None else Path(sys.base_prefix)
    candidates = [base / "lib" / binary.name, base / "lib64" / binary.name]
    if prefix is None:
        libdir = sysconfig.get_config_var("LIBDIR")
        if libdir:
            candidates.append(Path(libdir) / binary.name)
    for source in candidates:
        if source.is_file() and digest(source) == digest(binary):
            return source
    return None


def is_pe(binary):
    """Recognize actual PE bytes, including renamed extension modules."""
    with binary.open("rb") as stream:
        header = stream.read(64)
        if len(header) != 64 or header[:2] != b"MZ":
            return False
        offset = int.from_bytes(header[0x3c:0x40], "little")
        if offset < 64 or offset > binary.stat().st_size - 4:
            return False
        stream.seek(offset)
        return stream.read(4) == b"PE\0\0"


def pe_sections(binary):
    """Read PE section payload hashes; section headers can be rewritten in frozen EXEs."""
    data = binary.read_bytes()
    if len(data) < 64 or data[:2] != b"MZ":
        raise RuntimeError("not a PE: " + str(binary))
    offset = int.from_bytes(data[0x3c:0x40], "little")
    if offset + 24 > len(data) or data[offset:offset + 4] != b"PE\0\0":
        raise RuntimeError("invalid PE header: " + str(binary))
    count = int.from_bytes(data[offset + 6:offset + 8], "little")
    optional = int.from_bytes(data[offset + 20:offset + 22], "little")
    table = offset + 24 + optional
    if not count or table + count * 40 > len(data):
        raise RuntimeError("invalid PE section table: " + str(binary))
    sections = {}
    for index in range(count):
        header = data[table + index * 40:table + (index + 1) * 40]
        name = header[:8].rstrip(b"\0").decode("ascii")
        size = int.from_bytes(header[16:20], "little")
        start = int.from_bytes(header[20:24], "little")
        if not name or name in sections or start + size > len(data):
            raise RuntimeError("invalid PE section: " + str(binary))
        sections[name] = hashlib.sha256(data[start:start + size]).hexdigest()
    return sections


def generated_windows_executable_origin(executable, bootloader, spec, entry, arguments,
                                        expected_bootloader_sha256=PYINSTALLER_WINDOWS_BOOTLOADER_SHA256):
    """Track the generated EXE and exact bootloader input without asserting whole-file equality."""
    bootloader_hash = digest(bootloader)
    if bootloader_hash != expected_bootloader_sha256:
        raise RuntimeError("PyInstaller Windows bootloader hash differs from pinned wheel member")
    source_sections = pe_sections(bootloader)
    output_sections = pe_sections(executable)
    if source_sections.get(".text") is None or output_sections.get(".text") != source_sections[".text"]:
        raise RuntimeError("generated executable bootloader code section differs")
    unchanged = sorted(name for name in source_sections.keys() & output_sections.keys()
                       if source_sections[name] == output_sections[name])
    normalized = json.dumps(arguments, separators=(",", ":"), ensure_ascii=True).encode()
    return dict(path=executable.name, bytes=executable.stat().st_size, sha256=digest(executable),
                origin="generated PyInstaller Windows executable",
                source_artifact=PYINSTALLER_WINDOWS_WHEEL_URL,
                source_sha256=PYINSTALLER_WINDOWS_WHEEL_SHA256,
                source_member=PYINSTALLER_WINDOWS_BOOTLOADER_MEMBER,
                bootloader_sha256=bootloader_hash,
                unchanged_bootloader_sections=unchanged,
                entry_sha256=digest(entry), spec_sha256=digest(spec),
                invocation=arguments, invocation_sha256=hashlib.sha256(normalized).hexdigest(),
                notice="licenses/pyinstaller/00-COPYING.txt")


def remove_windows_os_ucrt(bundle):
    """Use the Windows 11 system UCRT instead of shipping copied OS DLLs."""
    internal = bundle / "_internal"
    removed = []
    for path in sorted(internal.rglob("*")):
        name = path.name.lower()
        if name.startswith("api-ms-win-") and name.endswith(".dll") and name not in WINDOWS_OS_UCRT_DLLS:
            raise RuntimeError("unreviewed Windows API-set DLL: " + str(path))
        if name not in WINDOWS_OS_UCRT_DLLS:
            continue
        if not path.is_file() or not is_pe(path):
            raise RuntimeError("unexpected Windows UCRT member: " + str(path))
        removed.append(dict(path=path.relative_to(bundle).as_posix(), bytes=path.stat().st_size,
                            sha256=digest(path), provided_by="Windows 11/Server 2022 OS UCRT/API-set"))
        path.unlink()
    print(f"Omitted {len(removed)} copied Windows OS UCRT/API-set DLLs; Windows 11/Server 2022 use system copies")
    return removed


def windows_pe_source_records():
    """Expand exact archive members; a filename or package family alone proves nothing."""
    catalog = json.loads(WINDOWS_PE_SOURCES.read_text(encoding="utf-8"))
    if catalog.get("schema_version") != 1:
        raise RuntimeError("Windows PE source catalog schema differs")
    records = {}
    for archive in catalog["archives"]:
        for binary in archive["binaries"]:
            path = binary["path"]
            if path in records or not path.startswith("_internal/") or ".." in Path(path).parts:
                raise RuntimeError("duplicate or unsafe Windows PE source path: " + path)
            records[path] = dict(origin="pinned Windows source archive member",
                                 source_artifact=archive["source_url"],
                                 source_sha256=archive["source_sha256"],
                                 source_member=binary["source_member"],
                                 source_member_sha256=binary["sha256"],
                                 notice=archive["notice"],
                                 notice_sha256=archive["notice_sha256"],
                                 **({"notice_source_member": archive["notice_source_member"]}
                                    if "notice_source_member" in archive else
                                    {"notice_source_url": archive["notice_source_url"]}))
    msvc = catalog["msvc_runtime"]
    if msvc["path"] in records:
        raise RuntimeError("duplicate Windows MSVC runtime source path")
    records[msvc["path"]] = dict(origin="Microsoft Visual C++ runtime redistributable",
                                source_artifact=msvc["source_artifact"],
                                source_sha256=msvc["source_sha256"],
                                source_container_member=msvc["source_container_member"],
                                source_container_sha256=msvc["source_container_sha256"],
                                source_member=msvc["source_member"],
                                source_member_sha256=msvc["sha256"],
                                notice=msvc["notice"],
                                notice_source_member=msvc["notice_source_member"],
                                notice_sha256=msvc["notice_sha256"])
    return catalog, records


def binary_origins(bundle, excluded_os_binaries=(), generated_windows_executable=None):
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
            python_source = python_runtime_source(binary)
            if python_source is not None:
                records.append(dict(path=relative, origin="Python", version=platform.python_version(),
                                    notice=notice_for("Python"), source=str(python_source.resolve())))
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
        # An executable or renamed module is as much a distributed PE as a
        # .dll/.pyd. Historical source candidates must match actual bytes and
        # the shipped notice; unknown copies remain visibly unresolved.
        _, source_records = windows_pe_source_records()
        matched = 0
        pending = []
        for binary in sorted(path for path in bundle.rglob("*") if path.is_file() and is_pe(path)):
            relative = binary.relative_to(bundle).as_posix()
            actual_sha256 = digest(binary)
            if relative == "voicetype-csc.exe" and generated_windows_executable is not None:
                if (generated_windows_executable["path"] != relative
                        or generated_windows_executable["sha256"] != actual_sha256
                        or generated_windows_executable["bytes"] != binary.stat().st_size):
                    raise RuntimeError("generated Windows executable origin differs from bundle")
                records.append(generated_windows_executable)
            else:
                source = source_records.get(relative)
                if source is not None and source["source_member_sha256"] == actual_sha256:
                    notice = bundle / source["notice"]
                    if not notice.is_file() or digest(notice) != source["notice_sha256"]:
                        raise RuntimeError("Windows PE notice differs from pinned source: " + relative)
                    records.append(dict(path=relative, bytes=binary.stat().st_size,
                                        sha256=actual_sha256, **source))
                    matched += 1
                else:
                    pending.append(relative)
                    records.append(dict(path=relative, bytes=binary.stat().st_size,
                                        sha256=actual_sha256,
                                        origin="Windows native binary; source/notice audit pending"))
        return dict(complete=False, binaries=records,
                    excluded_os_binaries=list(excluded_os_binaries),
                    source_catalog_sha256=digest(WINDOWS_PE_SOURCES),
                    source_catalog_matches=matched,
                    source_catalog_pending_paths=pending)
    return dict(complete=sys.platform == "linux", binaries=records,
                excluded_os_binaries=list(excluded_os_binaries))


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
        pyinstaller_arguments = ["--noconfirm", "--clean", "--onedir", "--noupx",
            "--name", "voicetype-csc", "--distpath", work / "dist", "--workpath", work / "build",
            "--specpath", work, "--collect-data", "opencc", "--collect-data", "pypinyin",
            "--collect-binaries", "onnxruntime", "--exclude-module", "torch", "--exclude-module", "onnx",
            "--exclude-module", "transformers", "--exclude-module", "huggingface_hub",
            ROOT / "scripts/voicetype_csc.py"]
        run(sys.executable, "-m", "PyInstaller", *pyinstaller_arguments)
        bundle = work / "bundle"
        # Resolve PyInstaller's same-directory library symlinks into ordinary
        # files; the app's version store never follows untrusted links.
        shutil.copytree(work / "dist/voicetype-csc", bundle, symlinks=False)
        excluded_os_binaries = remove_windows_os_ucrt(bundle) if sys.platform == "win32" else []
        versions = notices(bundle / "licenses")
        generated = None
        if sys.platform == "win32":
            catalog, _ = windows_pe_source_records()
            if digest(WINDOWS_MSVC_LICENSE) != catalog["msvc_runtime"]["notice_sha256"]:
                raise RuntimeError("pinned Microsoft runtime license differs from source")
            msvc_notice = bundle / catalog["msvc_runtime"]["notice"]
            msvc_notice.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(WINDOWS_MSVC_LICENSE, msvc_notice)
            bootloader = Path(metadata.distribution("pyinstaller").locate_file(
                PYINSTALLER_WINDOWS_BOOTLOADER_MEMBER))
            if not bootloader.is_file():
                raise RuntimeError("PyInstaller Windows bootloader missing from build environment")
            normalized_arguments = [str(argument).replace(str(work), "{work}").replace(str(ROOT), "{repo}")
                                    for argument in pyinstaller_arguments]
            generated = generated_windows_executable_origin(
                bundle / "voicetype-csc.exe", bootloader, work / "voicetype-csc.spec",
                ROOT / "scripts/voicetype_csc.py", normalized_arguments)
        origins = binary_origins(bundle, excluded_os_binaries, generated)
        models = bundle / "models"
        models.mkdir()
        for filename in ["model-int8-fused.onnx", "tokenizer.json"]:
            shutil.copyfile(prepared / filename, models / filename)
        provenance = bundle / "provenance"
        provenance.mkdir()
        if generated is not None:
            shutil.copyfile(work / "voicetype-csc.spec", provenance / "pyinstaller.spec")
            if digest(provenance / "pyinstaller.spec") != generated["spec_sha256"]:
                raise RuntimeError("stored PyInstaller spec differs from generated executable recipe")
            shutil.copyfile(WINDOWS_PE_SOURCES, provenance / "windows-pe-sources.json")
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
