#!/usr/bin/env python3
"""Catalog licenses for the Rust graphs and native bundles in this installer.

The app uses its build/default features. Linux and Windows also ship voicetyped
built with --no-default-features --features relocatable-runtime. Only normal/build edges
from those roots are included; Cargo.lock's unused or dev-only crates are not.
"""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def selected_license(expression):
    """Choose a compatible OR arm; retain every required AND arm/notice."""
    tokens = re.findall(r"\(|\)|\bAND\b|\bOR\b|\bWITH\b|/|[A-Za-z0-9.+-]+", expression)
    if not tokens or "".join(tokens).replace("/", "") != re.sub(r"\s+", "", expression).replace("/", ""):
        raise ValueError("unparsed license expression: " + expression)
    index = 0

    def atom():
        nonlocal index
        if index >= len(tokens):
            raise ValueError("incomplete license expression: " + expression)
        if tokens[index] == "(":
            index += 1
            result = alternative()
            if index >= len(tokens) or tokens[index] != ")":
                raise ValueError("unclosed license expression: " + expression)
            index += 1
            return result
        name = tokens[index]
        if name in {"AND", "OR", "/", ")", "WITH"}:
            raise ValueError("invalid license expression: " + expression)
        index += 1
        if index < len(tokens) and tokens[index] == "WITH":
            index += 1
            if index >= len(tokens):
                raise ValueError("missing license exception: " + expression)
            name += " WITH " + tokens[index]
            index += 1
        return name

    def conjunction():
        nonlocal index
        parts = [atom()]
        while index < len(tokens) and tokens[index] == "AND":
            index += 1
            parts.append(atom())
        return " AND ".join(parts)

    def alternative():
        nonlocal index
        parts = [conjunction()]
        while index < len(tokens) and tokens[index] in {"OR", "/"}:
            index += 1
            parts.append(conjunction())
        # MIT/Apache dual licenses are both compatible with this GPL-3 app.
        # Prefer MIT when offered, otherwise Apache, then retain the first arm.
        priority = ("MIT", "Apache-2.0", "ISC", "BSD-3-Clause", "Zlib")
        return min(parts, key=lambda value: next((i for i, name in enumerate(priority)
                                                  if name in value.split(" AND ")), len(priority)))

    choice = alternative()
    if index != len(tokens):
        raise ValueError("trailing license expression: " + expression)
    return choice


def cargo_graph(manifest, target, package, extra=()):
    command = ["cargo", "metadata", "--manifest-path", str(manifest), "--locked",
               "--format-version", "1", "--filter-platform", target, *extra]
    data = json.loads(subprocess.check_output(command, cwd=ROOT, env={**os.environ, "CARGO_BUILD_JOBS": "2"}))
    packages = {entry["id"]: entry for entry in data["packages"]}
    nodes = {entry["id"]: entry for entry in data["resolve"]["nodes"]}
    roots = [entry["id"] for entry in data["packages"] if entry["name"] == package]
    if len(roots) != 1:
        raise RuntimeError("expected one Rust package root: " + package)
    reached, pending = set(), roots[:]
    while pending:
        current = pending.pop()
        if current in reached:
            continue
        reached.add(current)
        for dependency in nodes[current]["deps"]:
            if any(kind["kind"] in (None, "build") for kind in dependency["dep_kinds"]):
                pending.append(dependency["pkg"])
    return {key: {**packages[key], "_resolved_features": sorted(nodes[key].get("features", []))}
            for key in reached}


def lock_checksums(path):
    result = {}
    for entry in tomllib.loads(path.read_text())["package"]:
        if "checksum" in entry:
            result[(entry["name"], entry["version"], entry["source"])] = entry["checksum"]
    return result


def vendored_archive(url, expected_sha256, filename):
    """Read an upstream crate from Cargo cache or project-local verified cache."""
    project_cache = ROOT / "desktop/target/rust-vendor-cache"
    candidates = list((Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")) /
                       "registry/cache").glob("*/" + filename))
    candidates.append(project_cache / filename)
    for candidate in candidates:
        if candidate.is_file():
            if digest(candidate) != expected_sha256:
                raise RuntimeError("vendored crate cached archive differs from source pin: " + filename)
            return candidate.read_bytes()
    with urllib.request.urlopen(url, timeout=60) as response:
        body = response.read()
    if hashlib.sha256(body).hexdigest() != expected_sha256:
        raise RuntimeError("vendored crate upstream archive hash mismatch: " + filename)
    project_cache.mkdir(parents=True, exist_ok=True)
    (project_cache / filename).write_bytes(body)
    return body


def verify_vendored_source(package, root=ROOT, pins=None, archive_bytes=None):
    """Prove every vendored file against a pinned crate plus explicit patch/omission."""
    if pins is None:
        pins = json.loads((root / "config/rust-vendor-sources.json").read_text())
    key = f"{package['name']}@{package['version']}"
    pin = pins.get(key)
    if pin is None:
        raise RuntimeError("unreviewed vendored crate: " + key)
    folder = Path(package["manifest_path"]).parent.resolve()
    if folder != (root / pin["path"]).resolve():
        raise RuntimeError("vendored crate path differs from pinned source: " + key)
    body = archive_bytes if archive_bytes is not None else vendored_archive(
        pin["archive_url"], pin["archive_sha256"], f"{package['name']}-{package['version']}.crate")
    if hashlib.sha256(body).hexdigest() != pin["archive_sha256"]:
        raise RuntimeError("vendored crate archive differs from source pin: " + key)
    upstream = {}
    prefix = key.replace("@", "-") + "/"
    with tarfile.open(fileobj=io.BytesIO(body), mode="r:gz") as archive:
        for member in archive:
            if not member.isfile():
                continue
            if not member.name.startswith(prefix):
                raise RuntimeError("vendored crate archive member outside package: " + key)
            relative = member.name[len(prefix):]
            if not relative or ".." in Path(relative).parts or relative in upstream:
                raise RuntimeError("vendored crate archive member invalid: " + key)
            upstream[relative] = archive.extractfile(member).read()
    omitted = set(pin["omitted"])
    added = set(pin["added"])
    patched = pin["patched"]
    if not omitted <= upstream.keys() or not set(patched) <= upstream.keys() or added & upstream.keys():
        raise RuntimeError("vendored crate declared diff differs from upstream: " + key)
    expected = (upstream.keys() - omitted) | added
    actual = {path.relative_to(folder).as_posix(): path for path in folder.rglob("*") if path.is_file()}
    if set(actual) != expected:
        raise RuntimeError("vendored crate file set differs from pinned archive: " + key)
    for relative, source in upstream.items():
        if relative in omitted:
            continue
        current = actual[relative].read_bytes()
        if relative in patched:
            if (hashlib.sha256(source).hexdigest() != patched[relative]["upstream_sha256"]
                    or hashlib.sha256(current).hexdigest() != patched[relative]["sha256"]):
                raise RuntimeError("vendored crate patch differs from pin: " + key)
        elif current != source:
            raise RuntimeError("vendored crate file differs from upstream: " + key + "/" + relative)
    return dict(path=pin["path"], source_url=pin["archive_url"],
                archive_sha256=pin["archive_sha256"], patched=patched,
                omitted=sorted(omitted), added={name: digest(actual[name]) for name in sorted(added)})


def license_sources(package, crate_checksum=None, vendor=None):
    folder = Path(package["manifest_path"]).parent
    archive = None
    if package["source"]:
        if not crate_checksum:
            raise RuntimeError("locked crate checksum missing: " + package["name"])
        archive = folder.parent.parent.parent / "cache" / folder.parent.name / f"{package['name']}-{package['version']}.crate"
        if not archive.is_file() or digest(archive) != crate_checksum:
            raise RuntimeError("crate archive differs from Cargo.lock: " + package["name"])
    found = {path for path in folder.iterdir() if path.is_file() and
             path.name.lower().startswith(("license", "licence", "copying", "notice", "copyright"))}
    if package.get("license_file"):
        license_file = Path(package["license_file"])
        found.add(license_file if license_file.is_absolute() else folder / license_file)
    if package["source"] is None and not found:
        choice = selected_license(package["license"])
        if choice != "GPL-3.0-only":
            raise RuntimeError(f"root GPL-3.0-only LICENSE cannot satisfy {package['name']} {choice}")
        found.add(ROOT / "LICENSE")
    fallback = None
    if not found:
        key = f"{package['name']}@{package['version']}"
        known = json.loads((ROOT / "config/rust-notice-fallbacks/sources.json").read_text())
        fallback = known.get(key)
        if fallback is None:
            return []
        vcs_file = folder / ".cargo_vcs_info.json"
        if not vcs_file.is_file() or json.loads(vcs_file.read_text())["git"]["sha1"] != fallback["vcs_sha"]:
            raise RuntimeError("Rust fallback differs from crate VCS revision: " + key)
        for filename, expected in fallback["files"].items():
            path = ROOT / "config/rust-notice-fallbacks" / filename
            if digest(path) != expected:
                raise RuntimeError("Rust fallback text hash mismatch: " + key)
            found.add(path)
    for path in found:
        if not path.is_file() or path.stat().st_size == 0:
            raise RuntimeError(f"missing/empty Rust license text: {package['name']} {path}")
    if package["source"] and fallback is None:
        with tarfile.open(archive, "r:gz") as crate:
            for path in found:
                relative = path.relative_to(folder).as_posix()
                member = crate.extractfile(f"{package['name']}-{package['version']}/{relative}")
                if member is None or hashlib.sha256(member.read()).hexdigest() != digest(path):
                    raise RuntimeError("Rust notice differs from locked crate: " + package["name"])
    return [(path, fallback["source_url"] if fallback else
             (f"crate:{package['name']}@{package['version']}/{path.relative_to(folder)}"
              if package["source"] else
              (f"crate:{package['name']}@{package['version']}/{path.relative_to(folder)} (vendored)" if vendor else
               ("repository:LICENSE" if path == ROOT / "LICENSE"
                else f"workspace:{path.relative_to(folder)}"))),
             fallback.get("note") if fallback else None)
            for path in sorted(found)]


def build(output, target, components):
    app = cargo_graph(ROOT / "desktop/src-tauri/Cargo.toml", target, "voicetype-desktop")
    graphs = {key: {"package": entry, "scope_features": {"desktop-app": entry["_resolved_features"]}}
              for key, entry in app.items()}
    locks = {**lock_checksums(ROOT / "desktop/Cargo.lock")}
    daemon = cargo_graph(ROOT / "voicetyped/Cargo.toml", target, "voicetyped",
                         ("--no-default-features", "--features", "relocatable-runtime"))
    locks.update(lock_checksums(ROOT / "voicetyped/Cargo.lock"))
    for key, entry in daemon.items():
        if key in graphs:
            graphs[key]["scope_features"]["owned-daemon"] = entry["_resolved_features"]
        else:
            graphs[key] = {"package": entry,
                           "scope_features": {"owned-daemon": entry["_resolved_features"]}}

    output.parent.mkdir(parents=True, exist_ok=True)
    if output.exists() or output.is_symlink():
        raise RuntimeError("notice output already exists")
    with tempfile.TemporaryDirectory(prefix=".rust-notices-", dir=output.parent) as temporary:
        stage = Path(temporary) / "bundle"
        stage.mkdir()
        records, files = [], []
        for key in sorted(graphs):
            package, scope_features = graphs[key]["package"], graphs[key]["scope_features"]
            expression = package["license"]
            if not expression:
                raise RuntimeError("missing Rust license expression: " + key)
            choice = selected_license(expression)
            source = package["source"]
            folder = Path(package["manifest_path"]).parent.resolve() if package.get("manifest_path") else None
            vendor = (verify_vendored_source(package) if folder is not None and
                      folder.is_relative_to(ROOT / "desktop/vendor") else None)
            checksum = (locks.get((package["name"], package["version"], source)) if source else
                        vendor["archive_sha256"] if vendor else None)
            if source and (not checksum or not source.startswith("registry+")):
                raise RuntimeError("unlocked/unsupported Rust source: " + key)
            slug = f"{package['name']}-{package['version']}-{(checksum or 'workspace')[:12]}"
            notices = []
            sources = license_sources(package, checksum, vendor) if vendor else license_sources(package, checksum)
            if not sources:
                raise RuntimeError("Rust source has no packaged license text: " + key)
            for number, (license_path, provenance, note) in enumerate(sources):
                filename = re.sub(r"[^A-Za-z0-9_.-]", "_", license_path.name)
                relative = f"packages/{slug}/{number:02d}-{filename}"
                destination = stage / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(license_path, destination)
                record = {"path": relative, "bytes": destination.stat().st_size,
                          "sha256": digest(destination)}
                files.append(record)
                notices.append({**record, "origin": provenance,
                                **({"source_note": note} if note else {})})
            records.append({"name": package["name"], "version": package["version"],
                            "source": source or ("vendored:" + vendor["source_url"] if vendor else "workspace"),
                            "crate_checksum": checksum,
                            "license_expression": expression, "selected_license": choice,
                            "scopes": sorted(scope_features), "scope_features": scope_features,
                            "notices": notices,
                            **({"vendored_source": vendor} if vendor else {})})

        native = []
        for name, directory in components:
            catalog = directory / "manifest.json"
            if not catalog.is_file():
                raise RuntimeError("native catalog missing: " + name)
            data = json.loads(catalog.read_text())
            license_files = [entry for entry in data["files"]
                             if entry["path"].lower().startswith("licenses/")
                             or "license" in entry["path"].lower()]
            if not license_files:
                raise RuntimeError("native component has no cataloged notice: " + name)
            for entry in license_files:
                path = directory / entry["path"]
                if not path.is_file() or digest(path) != entry["sha256"]:
                    raise RuntimeError("native notice differs from catalog: " + name)
            native.append({"name": name, "id": data["id"], "catalog_sha256": digest(catalog),
                           "notice_count": len(license_files)})

        manifest = {"schema_version": 1, "target": target,
                    "roots": ["desktop-app", "owned-daemon"],
                    "locks": {"desktop": digest(ROOT / "desktop/Cargo.lock"),
                              "owned-daemon": digest(ROOT / "voicetyped/Cargo.lock")},
                    "packages": records, "files": sorted(files, key=lambda item: item["path"]),
                    "native_components": native, "complete": True}
        (stage / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        stage.rename(output)
    print(f"Rust notices: {len(records)} locked graph packages, {len(files)} texts, {len(native)} native catalogs")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--target", required=True, choices=("x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"))
    parser.add_argument("--component", action="append", default=[], metavar="NAME=PATH")
    args = parser.parse_args()
    components = []
    for raw in args.component:
        if "=" not in raw:
            parser.error("--component requires NAME=PATH")
        name, path = raw.split("=", 1)
        if name not in {"runtime", "input", "spelling", "opencc"}:
            parser.error("unknown native component: " + name)
        components.append((name, Path(path).resolve()))
    required = {"runtime", "input", "spelling"} if args.target.endswith("linux-gnu") else {"runtime", "opencc", "spelling"}
    if {name for name, _ in components} != required:
        parser.error("native component set differs from installer")
    build(args.output.resolve(), args.target, components)


if __name__ == "__main__":
    main()
