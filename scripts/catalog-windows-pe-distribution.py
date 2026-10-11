#!/usr/bin/env python3
"""Resolve the actual Windows NSIS PE set against the already pinned scope catalogs.

The output is external to the installer: hashing the generated setup inside its
own payload would be circular. Generated EXEs retain their input/recipe/output
chain; their resources and archives need not equal an unmodified stub.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import zipfile


NSIS_SOURCE = "https://github.com/tauri-apps/binary-releases/releases/download/nsis-3.11/nsis-3.11.zip"
NSIS_SHA256 = "c7d27f780ddb6cffb4730138cd1591e841f4b7edb155856901cdf5f214394fa1"
SETUP_STUB = "nsis-3.11/Stubs/lzma_solid-x86-unicode"
UNINSTALL_ICON = "nsis-3.11/Stubs/uninst"


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def read_json(path):
    require(path.is_file(), "required provenance missing: " + str(path))
    return json.loads(path.read_text(encoding="utf-8"))


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


def pe_sections(data):
    require(data[:2] == b"MZ" and len(data) >= 64, "NSIS stub is not PE")
    offset = int.from_bytes(data[0x3c:0x40], "little")
    require(offset + 24 <= len(data) and data[offset:offset + 4] == b"PE\0\0",
            "NSIS PE header differs")
    count = int.from_bytes(data[offset + 6:offset + 8], "little")
    optional = int.from_bytes(data[offset + 20:offset + 22], "little")
    table = offset + 24 + optional
    require(0 < count <= 32 and table + count * 40 <= len(data), "NSIS section table differs")
    sections = {}
    for index in range(count):
        header = data[table + index * 40:table + (index + 1) * 40]
        name = header[:8].rstrip(b"\0").decode("ascii")
        size = int.from_bytes(header[16:20], "little")
        start = int.from_bytes(header[20:24], "little")
        require(name and name not in sections and start + size <= len(data), "NSIS section differs")
        sections[name] = hashlib.sha256(data[start:start + size]).hexdigest()
    return sections


def actual_pe_set(extracted):
    rows = {}
    folded = set()
    for path in extracted.rglob("*"):
        if not path.is_file():
            continue
        require(not path.is_symlink(), "NSIS extraction contains a symlink")
        if not is_pe(path):
            continue
        name = path.relative_to(extracted).as_posix()
        require(name.casefold() not in folded, "NSIS PE paths collide by Windows case: " + name)
        folded.add(name.casefold())
        rows[name] = {"path": name, "bytes": path.stat().st_size, "sha256": sha256(path)}
    return rows


def compare_inventory(actual, expected):
    require(len(expected) == len({row["path"].casefold() for row in expected}),
            "source PE paths collide by Windows case")
    sources = {row["path"]: row for row in expected}
    require(set(actual) == set(sources), "NSIS PE set differs; missing=" +
            str(sorted(set(sources) - set(actual))) + " extra=" + str(sorted(set(actual) - set(sources))))
    for name, observed in actual.items():
        source = sources[name]
        require(observed["bytes"] == source["bytes"] and observed["sha256"] == source["sha256"],
                "NSIS PE bytes differ: " + name)


def manifest_pe_rows(manifest, prefix):
    return [{"path": prefix + row["path"], "bytes": row["bytes"], "sha256": row["sha256"]}
            for row in manifest["files"] if row["path"].lower().endswith((".exe", ".dll", ".pyd"))]


def verify_manifest_files(manifest, root):
    """Resolve the existing scope catalog's shipped notices and other files."""
    seen = set()
    for row in manifest["files"]:
        name = row["path"]
        relative = Path(name)
        require(name and not relative.is_absolute() and ".." not in relative.parts
                and name.casefold() not in seen, "unsafe or duplicate manifest path: " + name)
        seen.add(name.casefold())
        path = root / relative
        require(path.is_file() and not path.is_symlink()
                and path.stat().st_size == row["bytes"] and sha256(path) == row["sha256"],
                "manifest file missing or bytes differ: " + name)


def check_csc_origins(extracted):
    root = extracted / "spelling"
    origins = read_json(root / "provenance/binary-origins.json")
    require(origins.get("complete") is True, "CSC source/notice catalog incomplete")
    require(origins.get("msvc_redistribution_eligibility") == "not_assessed",
            "CSC technical source and MSVC eligibility boundary missing")
    require(sha256(root / "provenance/windows-pe-sources.json") == origins["source_catalog_sha256"],
            "CSC source catalog bytes differ")
    generated = 0
    rows = []
    for row in origins["binaries"]:
        path = root / row["path"]
        require(path.is_file() and is_pe(path), "CSC listed PE missing: " + row["path"])
        notice = root / row["notice"]
        require(notice.is_file() and sha256(notice) == row["notice_sha256"],
                "CSC native notice missing or differs: " + row["path"])
        official = row.get("microsoft_runtime_source")
        if official:
            microsoft_notice = root / official["notice"]
            require(official["sha256"] == row["sha256"] and microsoft_notice.is_file()
                    and sha256(microsoft_notice) == official["notice_sha256"]
                    and official["source_artifact"] and official["source_sha256"]
                    and official["source_container_sha256"] and official["source_member"],
                    "CSC Microsoft runtime source/notice differs: " + row["path"])
        if row["path"] == "voicetype-csc.exe":
            generated += 1
            spec = root / "provenance/pyinstaller.spec"
            require(spec.is_file() and sha256(spec) == row["spec_sha256"]
                    and row["bootloader_sha256"] and row["entry_sha256"]
                    and row["invocation_sha256"] and ".text" in row["unchanged_bootloader_sections"],
                    "generated CSC recipe/bootloader evidence missing")
        else:
            require(row["source_member_sha256"] == row["sha256"]
                    and row["source_artifact"] and row["source_sha256"] and row["source_member"],
                    "CSC source member evidence missing: " + row["path"])
        rows.append({"path": "spelling/" + row["path"], "bytes": row["bytes"],
                     "sha256": row["sha256"]})
    require(generated == 1, "generated CSC executable count differs")
    return rows, origins


def audit(extracted, installed, setup, target, repo, source_commit):
    require(re.fullmatch(r"[0-9a-f]{40}", source_commit) is not None,
            "full source commit is required")
    require(is_pe(setup), "outer NSIS setup is not PE")
    actual = actual_pe_set(extracted)
    plugins = read_json(target / "installer-notices/plugin-origins.json")
    plugin_audit = read_json(target / "installer-pe-audit.json")
    require(plugins["source_ready"] and plugin_audit["plugins_verified"]
            and plugin_audit["installed_notices_verified"], "plugin source/installed notice gate incomplete")
    require(plugin_audit["outer_installer"]["sha256"] == sha256(setup)
            and plugin_audit["outer_installer"]["bytes"] == setup.stat().st_size,
            "setup output differs from plugin audit")
    expected = [{"path": row["path"], "bytes": row["bytes"], "sha256": row["sha256"]}
                for row in plugins["plugins"]]
    csc_rows, csc = check_csc_origins(extracted)
    expected.extend(csc_rows)
    catalogs = {}
    for scope, extracted_name in (("google", "google/manifest.json"),
                                  ("runtime", "runtime/manifest.json"),
                                  ("opencc", "opencc-manifest.json")):
        file = target / scope / "manifest.json"
        unpacked = extracted / extracted_name
        require(file.is_file() and unpacked.is_file() and file.read_bytes() == unpacked.read_bytes(),
                scope + " installed source catalog differs")
        catalogs[scope] = read_json(file)
    require(catalogs["google"]["id"] == "google-helpers" and
            catalogs["opencc"]["id"] == "opencc" and
            catalogs["runtime"]["id"] == "nano-runtime", "non-CSC source catalog differs")
    for name, root in (("google", extracted / "google"),
                       ("runtime", extracted / "runtime"), ("opencc", extracted)):
        verify_manifest_files(catalogs[name], root)
    expected.extend(manifest_pe_rows(catalogs["google"], "google/"))
    expected.extend(manifest_pe_rows(catalogs["runtime"], "runtime/"))
    expected.extend(manifest_pe_rows(catalogs["opencc"], ""))
    rust = read_json(target / "rust-notices/manifest.json")
    require(rust["complete"] is True and rust["target"] == "x86_64-pc-windows-msvc",
            "Windows Rust notice graph incomplete")
    require(rust["locks"]["desktop"] == sha256(repo / "desktop/Cargo.lock")
            and rust["locks"]["owned-daemon"] == sha256(repo / "voicetyped/Cargo.lock"),
            "Rust notice source locks differ")
    native = {row["name"]: row for row in rust["native_components"]}
    for name in ("google", "runtime", "opencc", "spelling"):
        if name == "google":
            require(any(row["path"] == "LICENSE.txt" for row in catalogs["google"]["files"]),
                    "Google helper license record missing")
            continue
        require(name in native and native[name]["catalog_sha256"] == sha256(target / name / "manifest.json")
                and native[name]["notice_count"] > 0, "native source/notice link differs: " + name)
    native_recipe = read_json(target / "runtime/build-provenance.json")
    require(native_recipe["platform"] == "windows-x86_64"
            and native_recipe["provider"] == "cpu"
            and native_recipe["files"], "Windows native build provenance missing")
    main = extracted / "voicetype-desktop.exe"
    require(main.is_file() and is_pe(main), "generated desktop PE missing")
    expected.append({"path": main.name, "bytes": main.stat().st_size, "sha256": sha256(main)})
    compare_inventory(actual, expected)
    require(len(actual) == 60, "Windows inner PE count differs from reviewed scopes")
    archive = target / "installer-source-cache/nsis-3.11.zip"
    require(archive.is_file() and sha256(archive) == NSIS_SHA256,
            "pinned NSIS source archive missing or differs")
    with zipfile.ZipFile(archive) as source:
        setup_stub = source.read(SETUP_STUB)
        uninstall_icon = source.read(UNINSTALL_ICON)
        require(source.read("nsis-3.11/COPYING") ==
                (target / "installer-notices/NSIS-COPYING.txt").read_bytes(),
                "NSIS stub notice differs")
    stub_sections = pe_sections(setup_stub)
    setup_sections = pe_sections(setup.read_bytes())
    unchanged = [name for name in (".text", ".rdata", ".data")
                 if name in stub_sections and setup_sections.get(name) == stub_sections[name]]
    require(unchanged == [".text", ".rdata", ".data"], "generated NSIS setup stub code/data differs")
    uninstallers = [p for p in installed.rglob("*") if p.is_file() and is_pe(p)
                    and p.name.lower().startswith("uninstall") and p.suffix.lower() == ".exe"]
    require(len(uninstallers) == 1 and uninstallers[0].parent == installed,
            "installed generated NSIS uninstaller missing or duplicated")
    require((extracted / "LICENSE.txt").is_file(), "desktop distribution license missing")
    recipe = {path: sha256(repo / path) for path in (
        "desktop/Cargo.lock", "voicetyped/Cargo.lock", "desktop/package-lock.json",
        "desktop/src-tauri/tauri.conf.json", "desktop/src-tauri/tauri.windows.conf.json",
        "desktop/src-tauri/windows/upgrade-legacy-ucrt.nsh",
        "scripts/build-desktop-google.py", "scripts/build-desktop-opencc.py",
        "scripts/build-desktop-native-windows.py", ".github/workflows/desktop-preview.yml")}
    return {"schema_version": 1, "scope": "Windows NSIS setup and installed generated uninstaller",
            "source_commit": source_commit, "complete": True,
            "msvc_redistribution_eligibility": "not_assessed",
            "inner_pe_count": len(actual), "pe_count_including_outer_setup": len(actual) + 1,
            "inner_pe": [dict(actual[name], scope=("plugin" if name.startswith("$PLUGINSDIR/") else
                                                        "spelling" if name.startswith("spelling/") else
                                                        "google" if name.startswith("google/") else
                                                        "runtime" if name.startswith("runtime/") else
                                                        "desktop" if name == "voicetype-desktop.exe" else "opencc"))
                         for name in sorted(actual)],
            "source_catalogs": {name: sha256(target / name / "manifest.json")
                                for name in ("google", "runtime", "opencc", "spelling")},
            "csc_binary_origins_sha256": sha256(extracted / "spelling/provenance/binary-origins.json"),
            "plugin_origins_sha256": sha256(target / "installer-notices/plugin-origins.json"),
            "rust_notices_sha256": sha256(target / "rust-notices/manifest.json"),
            "recipe_files_sha256": recipe,
            "generated_desktop": {"path": main.name, "bytes": main.stat().st_size,
                                  "sha256": sha256(main), "source_commit": source_commit,
                                  "cargo_lock_sha256": rust["locks"]["desktop"]},
            "generated_google_helpers": {"source_commit": source_commit,
                                         "cargo_lock_sha256": rust["locks"]["desktop"],
                                         "output_paths": [row["path"] for row in manifest_pe_rows(catalogs["google"], "google/")]},
            "generated_setup": {"path": setup.name, "bytes": setup.stat().st_size,
                                "sha256": sha256(setup), "source_artifact": NSIS_SOURCE,
                                "source_sha256": NSIS_SHA256, "source_member": SETUP_STUB,
                                "source_member_sha256": hashlib.sha256(setup_stub).hexdigest(),
                                "unchanged_stub_sections": unchanged,
                                "generated_resources_and_overlay": True},
            "installed_generated_uninstaller": {
                "path": uninstallers[0].relative_to(installed).as_posix(),
                "bytes": uninstallers[0].stat().st_size, "sha256": sha256(uninstallers[0]),
                "source_artifact": NSIS_SOURCE, "source_sha256": NSIS_SHA256,
                "generation": "NSIS WriteUninstaller from the installed setup",
                "generating_setup_sha256": sha256(setup),
                "uninstaller_icon_source_member": UNINSTALL_ICON,
                "uninstaller_icon_sha256": hashlib.sha256(uninstall_icon).hexdigest()}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("extracted", type=Path)
    parser.add_argument("installed", type=Path)
    parser.add_argument("setup", type=Path)
    parser.add_argument("target", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[1]
    source_commit = os.environ.get("GITHUB_SHA", "")
    try:
        report = audit(args.extracted, args.installed, args.setup, args.target, repo,
                       source_commit)
    except (RuntimeError, OSError, KeyError, ValueError, TypeError, IndexError) as error:
        report = {"schema_version": 1,
                  "scope": "Windows NSIS setup and installed generated uninstaller",
                  "source_commit": source_commit, "complete": False,
                  "msvc_redistribution_eligibility": "not_assessed",
                  "unresolved": [f"{type(error).__name__}: {error}"[:240]]}
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    if report["complete"]:
        print(f"PASS: {report['pe_count_including_outer_setup']} setup PE and installed generated uninstaller resolved")
    else:
        print("INCOMPLETE Windows PE provenance: " + report["unresolved"][0])


if __name__ == "__main__":
    main()
