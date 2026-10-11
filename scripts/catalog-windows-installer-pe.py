#!/usr/bin/env python3
"""Prepare pinned NSIS plugin notices, then audit the actual extracted installer.

This is the installer-only plugin scope. The generated setup EXE and installed
desktop/OpenCC/CSC PEs require their own origin catalogs before the whole
Windows distribution can be called complete.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import urllib.request
import zipfile


NSIS = (
    "https://github.com/tauri-apps/binary-releases/releases/download/nsis-3.11/nsis-3.11.zip",
    "c7d27f780ddb6cffb4730138cd1591e841f4b7edb155856901cdf5f214394fa1",
    "ef7ff767e5cbd9edd22add3a32c9b8f4500bb10d",
)
TAURI_UTILS = (
    "https://github.com/tauri-apps/nsis-tauri-utils/releases/download/"
    "nsis_tauri_utils-v0.5.3/nsis_tauri_utils.dll",
    "5ba143b5db4a87d32d6e7802e033330aae56cbceabe0d1e3ba41948385ad4709",
    "75197fee3c6a814fe035788d1c34ead39349b860",
)
TAURI_LICENSES = {
    "LICENSE_MIT": (
        "https://raw.githubusercontent.com/tauri-apps/nsis-tauri-utils/"
        "nsis_tauri_utils-v0.5.3/LICENSE_MIT",
        "1c1020fa10a6bf318717e82c911bcc54ebdfb9bb280460ae332bcb2f82f57fbe",
    ),
    "LICENSE_APACHE-2.0": (
        "https://raw.githubusercontent.com/tauri-apps/nsis-tauri-utils/"
        "nsis_tauri_utils-v0.5.3/LICENSE_APACHE-2.0",
        "0d542e0c8804e39aa7f37eb00da5a762149dc682d7829451287e11b938e94594",
    ),
}
NSIS_MEMBERS = {
    "LangDLL.dll": "4cf25411f28f639f72156c24b0f66ea42f5aee5973f6c137d901da6ae42d5b7e",
    "NSISdl.dll": "ddf40229dd9d6b268902d8dea88c8a04aacf1af218dd29f6dcd35babc54ac08d",
    "StartMenu.dll": "fe330320459637d8dd2b5d84d07925c38ec264152ffd3b1760a4d44d76536caa",
    "System.dll": "8b4c47c4cf5e76ec57dd5a050d5acd832a0d532ee875d7b44f6cdaf68f90d37c",
    "nsDialogs.dll": "b1350f487692057c8ffde75dcc55287a52a3272240d4d4912f24464b27551fc0",
}
NSIS_NOTICES = {
    "nsis-3.11/COPYING": ("NSIS-COPYING.txt", "e7dd514003ab96cb3ddccbc028fe5c795fccf57dc41f21cfb9d4dd16ead23bf5"),
    "nsis-3.11/Docs/NSISdl/License.txt": ("NSISdl-License.txt", "326e587f36ca58e14bd09cca153f9e69f1c5ed701189aa47d2d0627af7e16280"),
}


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def pinned_source(cache, filename, url, expected_sha256, expected_sha1=None):
    path = cache / filename
    if not path.is_file():
        with urllib.request.urlopen(url, timeout=60) as response:
            data = response.read()
        if sha256(data) != expected_sha256:
            raise RuntimeError("source archive SHA-256 mismatch: " + filename)
        if expected_sha1 and hashlib.sha1(data).hexdigest() != expected_sha1:
            raise RuntimeError("Tauri-pinned source SHA-1 mismatch: " + filename)
        cache.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    data = path.read_bytes()
    if sha256(data) != expected_sha256:
        raise RuntimeError("source archive SHA-256 mismatch: " + filename)
    if expected_sha1 and hashlib.sha1(data).hexdigest() != expected_sha1:
        raise RuntimeError("Tauri-pinned source SHA-1 mismatch: " + filename)
    return data


def prepare(output, cache):
    """Record exact source members and stage terms for bundling with the app."""
    if output.exists():
        raise RuntimeError("installer notice output must be new")
    archive = pinned_source(cache, "nsis-3.11.zip", *NSIS)
    utils = pinned_source(cache, "nsis_tauri_utils-v0.5.3.dll", *TAURI_UTILS)
    notices = {}
    records = []
    with zipfile.ZipFile(io.BytesIO(archive)) as source:
        for member, (name, expected) in NSIS_NOTICES.items():
            body = source.read(member)
            if sha256(body) != expected:
                raise RuntimeError("NSIS notice hash mismatch: " + member)
            notices[name] = body
        for name, expected in NSIS_MEMBERS.items():
            member = "nsis-3.11/Plugins/x86-unicode/" + name
            body = source.read(member)
            if sha256(body) != expected:
                raise RuntimeError("NSIS plugin member hash mismatch: " + name)
            records.append(dict(path="$PLUGINSDIR/" + name, bytes=len(body), sha256=expected,
                                source_artifact=NSIS[0], source_sha256=NSIS[1],
                                source_member=member, source_member_sha256=expected,
                                notice=["NSIS-COPYING.txt"] + (["NSISdl-License.txt"] if name == "NSISdl.dll" else [])))
    for filename, (url, expected) in TAURI_LICENSES.items():
        body = pinned_source(cache, "nsis-tauri-utils-" + filename, url, expected)
        notices["nsis-tauri-utils-" + filename + ".txt"] = body
    records.append(dict(path="$PLUGINSDIR/nsis_tauri_utils.dll", bytes=len(utils), sha256=TAURI_UTILS[1],
                        source_artifact=TAURI_UTILS[0], source_sha256=TAURI_UTILS[1],
                        source_member="release binary", source_member_sha256=TAURI_UTILS[1],
                        notice=["nsis-tauri-utils-LICENSE_MIT.txt",
                                "nsis-tauri-utils-LICENSE_APACHE-2.0.txt"]))
    output.mkdir(parents=True)
    for name, body in notices.items():
        (output / name).write_bytes(body)
    catalog = dict(scope="NSIS installer plugins only", source_ready=True,
                   actual_installer_verified=False, whole_windows_distribution_complete=False,
                   plugins=sorted(records, key=lambda record: record["path"]))
    (output / "plugin-origins.json").write_text(json.dumps(catalog, indent=2) + "\n")
    return catalog


def is_pe(path):
    with path.open("rb") as stream:
        header = stream.read(64)
        if len(header) < 64 or header[:2] != b"MZ":
            return False
        offset = int.from_bytes(header[0x3c:0x40], "little")
        if offset < 64 or offset > path.stat().st_size - 4:
            return False
        stream.seek(offset)
        return stream.read(4) == b"PE\0\0"


def verify_plugin_inventory(extracted_installer, records):
    """Require both actual PE set and every byte hash from a fresh extraction."""
    plugins = extracted_installer / "$PLUGINSDIR"
    if not plugins.is_dir():
        raise RuntimeError("NSIS plugin extraction missing")
    actual = {path.relative_to(extracted_installer).as_posix(): path
              for path in plugins.rglob("*") if path.is_file() and is_pe(path)}
    expected = {record["path"]: record for record in records}
    if len(expected) != len(records) or set(actual) != set(expected):
        raise RuntimeError("installer plugin PE set differs from source catalog")
    for name, path in actual.items():
        record = expected[name]
        if path.stat().st_size != record["bytes"] or sha256(path.read_bytes()) != record["sha256"]:
            raise RuntimeError("installer plugin bytes differ from source archive: " + name)
    return sorted(actual)


def verify_installed_notices(installation, prepared):
    """Verify the notice subtree really shipped with the installed app."""
    expected = {path.name: path for path in prepared.iterdir() if path.is_file()}
    candidates = list(installation.rglob("licenses/installer/plugin-origins.json"))
    if len(candidates) != 1:
        raise RuntimeError("installed installer notice catalog missing or duplicated")
    installed = candidates[0].parent
    actual = {path.name: path for path in installed.iterdir() if path.is_file()}
    if set(actual) != set(expected):
        raise RuntimeError("installed installer notice set differs from prepared catalog")
    for name, path in actual.items():
        if sha256(path.read_bytes()) != sha256(expected[name].read_bytes()):
            raise RuntimeError("installed installer notice bytes differ: " + name)
    return len(actual)


def verify(extracted_installer, catalog_path, installer, output, installed=None):
    catalog = json.loads(catalog_path.read_text())
    if catalog["scope"] != "NSIS installer plugins only" or not catalog["source_ready"]:
        raise RuntimeError("installer plugin source catalog not ready")
    observed = verify_plugin_inventory(extracted_installer, catalog["plugins"])
    if not is_pe(installer):
        raise RuntimeError("outer NSIS installer is not PE")
    installed_notices = verify_installed_notices(installed, catalog_path.parent) if installed else None
    evidence = dict(scope="NSIS installer plugins only", plugins_verified=True,
                    installed_notices_verified=installed_notices is not None,
                    observed_pe_paths=observed, outer_installer=dict(path=installer.name,
                        bytes=installer.stat().st_size, sha256=sha256(installer.read_bytes()),
                        origin="generated NSIS executable; provenance pending"),
                    whole_windows_distribution_complete=False)
    output.write_text(json.dumps(evidence, indent=2) + "\n")
    return evidence


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="operation", required=True)
    prep = sub.add_parser("prepare")
    prep.add_argument("output", type=Path)
    prep.add_argument("--cache", required=True, type=Path)
    check = sub.add_parser("verify")
    check.add_argument("extracted_installer", type=Path)
    check.add_argument("catalog", type=Path)
    check.add_argument("installer", type=Path)
    check.add_argument("output", type=Path)
    check.add_argument("--installed", type=Path, help="Require notices in the installed App")
    args = parser.parse_args()
    if args.operation == "prepare":
        prepare(args.output, args.cache)
    else:
        verify(args.extracted_installer, args.catalog, args.installer, args.output, args.installed)


if __name__ == "__main__":
    main()
