"""Rust notice graph and installed-file boundary fixtures."""
import hashlib
import io
import json
from pathlib import Path
import runpy
import tarfile
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
BUILDER = runpy.run_path(str(ROOT / "scripts/build-desktop-rust-notices.py"))
INSTALLED = runpy.run_path(str(ROOT / "desktop/tests/installed_rust_notices.py"))


class RustNoticeTests(unittest.TestCase):
    def test_vendored_crate_must_match_original_archive_except_exact_patch(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            folder = root / "desktop/vendor/example-1.0"
            (folder / "src").mkdir(parents=True)
            original = {"Cargo.toml": b"[package]\nname='example'\nversion='1.0'\n",
                        "LICENSE.md": b"MIT terms",
                        "src/windows.rs": b"original flags",
                        "Cargo.lock": b"standalone lock"}
            archive = io.BytesIO()
            with tarfile.open(fileobj=archive, mode="w:gz") as tar:
                for name, body in original.items():
                    info = tarfile.TarInfo("example-1.0/" + name)
                    info.size = len(body)
                    tar.addfile(info, io.BytesIO(body))
            for name, body in original.items():
                if name == "Cargo.lock":
                    continue
                path = folder / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"patched flags" if name == "src/windows.rs" else body)
            (folder / "VOICETYPE-PROVENANCE.md").write_text("reviewed patch")
            pins = {"example@1.0": {
                "path": "desktop/vendor/example-1.0",
                "archive_url": "https://static.crates.io/crates/example/example-1.0.crate",
                "archive_sha256": hashlib.sha256(archive.getvalue()).hexdigest(),
                "omitted": ["Cargo.lock"], "added": ["VOICETYPE-PROVENANCE.md"],
                "patched": {"src/windows.rs": {
                    "upstream_sha256": hashlib.sha256(original["src/windows.rs"]).hexdigest(),
                    "sha256": hashlib.sha256(b"patched flags").hexdigest()}}}}
            package = {"name": "example", "version": "1.0", "source": None,
                       "manifest_path": str(folder / "Cargo.toml")}
            result = BUILDER["verify_vendored_source"](package, root, pins, archive.getvalue())
            self.assertEqual(result["archive_sha256"], pins["example@1.0"]["archive_sha256"])
            (folder / "LICENSE.md").write_bytes(b"changed terms")
            with self.assertRaisesRegex(RuntimeError, "vendored crate"):
                BUILDER["verify_vendored_source"](package, root, pins, archive.getvalue())

    def test_windows_builder_catalogs_owned_daemon_and_runtime(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative in ("desktop/Cargo.lock", "voicetyped/Cargo.lock"):
                lock = root / relative
                lock.parent.mkdir(parents=True, exist_ok=True)
                lock.write_text("fixture lock")
            notice = root / "LICENSE"
            notice.write_text("fixture notice")
            components = []
            for name in ("runtime", "opencc", "spelling"):
                native = root / name
                (native / "licenses").mkdir(parents=True)
                license_file = native / "licenses/NOTICE"
                license_file.write_text("fixture " + name)
                checksum = hashlib.sha256(license_file.read_bytes()).hexdigest()
                (native / "manifest.json").write_text(json.dumps({"id": name, "files": [
                    {"path": "licenses/NOTICE", "sha256": checksum}]}))
                components.append((name, native))
            def graph(_manifest, target, package, _extra=()):
                self.assertEqual(target, "x86_64-pc-windows-msvc")
                return {package: {"name": package, "version": "1", "source": None,
                                  "license": "GPL-3.0-only", "_resolved_features": []}}
            output = root / "output"
            with mock.patch.dict(BUILDER["build"].__globals__, ROOT=root, cargo_graph=graph,
                                 lock_checksums=lambda _: {},
                                 license_sources=lambda _package, _checksum: [(notice, "fixture", None)]):
                BUILDER["build"](output, "x86_64-pc-windows-msvc", components)
            manifest = json.loads((output / "manifest.json").read_text())
            self.assertEqual(set(manifest["roots"]), {"desktop-app", "owned-daemon"})
            self.assertEqual({item["name"] for item in manifest["native_components"]},
                             {"runtime", "opencc", "spelling"})
            self.assertEqual({item["name"] for item in manifest["packages"]},
                             {"voicetype-desktop", "voicetyped"})

    def test_target_graph_keeps_normal_and_build_edges_but_not_dev(self):
        packages = [{"id": name, "name": name} for name in ("voicetype-desktop", "normal", "build", "dev")]
        nodes = [{"id": "voicetype-desktop", "deps": [
            {"pkg": name, "dep_kinds": [{"kind": kind}]}
            for name, kind in (("normal", None), ("build", "build"), ("dev", "dev"))]},
            *({"id": name, "deps": []} for name in ("normal", "build", "dev"))]
        encoded = json.dumps({"packages": packages, "resolve": {"nodes": nodes}}).encode()
        with mock.patch("subprocess.check_output", return_value=encoded):
            graph = BUILDER["cargo_graph"](ROOT / "desktop/Cargo.toml",
                                            "x86_64-unknown-linux-gnu", "voicetype-desktop")
        self.assertEqual({p["name"] for p in graph.values()},
                         {"voicetype-desktop", "normal", "build"})

    def test_license_selection_keeps_mandatory_and_arm(self):
        choose = BUILDER["selected_license"]
        self.assertEqual(choose("MIT OR Apache-2.0"), "MIT")
        self.assertEqual(choose("(MIT OR Apache-2.0) AND Unicode-3.0"),
                         "MIT AND Unicode-3.0")
        with self.assertRaises(ValueError):
            choose("MIT OR")

    def test_locked_crate_archive_and_loose_notice_must_match(self):
        with tempfile.TemporaryDirectory() as directory:
            registry = Path(directory) / "registry"
            source = registry / "src/index/example-1.0"
            source.mkdir(parents=True)
            license_file = source / "LICENSE-MIT"
            license_file.write_text("The crate license")
            archive = registry / "cache/index/example-1.0.crate"
            archive.parent.mkdir(parents=True)
            with tarfile.open(archive, "w:gz") as tar:
                tar.add(license_file, arcname="example-1.0/LICENSE-MIT")
            package = {"name": "example", "version": "1.0",
                       "manifest_path": str(source / "Cargo.toml"),
                       "source": "registry+https://github.com/rust-lang/crates.io-index",
                       "license_file": None}
            checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
            self.assertEqual(len(BUILDER["license_sources"](package, checksum)), 1)
            license_file.write_text("tampered loose source")
            with self.assertRaisesRegex(RuntimeError, "differs from locked crate"):
                BUILDER["license_sources"](package, checksum)

    def test_workspace_root_gpl_notice_rejects_mit_declaration(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            workspace = root / "daemon"
            workspace.mkdir()
            (root / "LICENSE").write_text("GNU GENERAL PUBLIC LICENSE\nVersion 3")
            package = {"name": "daemon", "version": "0.1.0",
                       "manifest_path": str(workspace / "Cargo.toml"),
                       "source": None, "license": "MIT", "license_file": None}
            with mock.patch.dict(BUILDER["license_sources"].__globals__, ROOT=root):
                with self.assertRaisesRegex(RuntimeError, "GPL.*MIT|MIT.*GPL"):
                    BUILDER["license_sources"](package)

    def test_installed_catalog_requires_exact_member_set_and_native_catalog(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = root / "installed"
            notices = installed / "licenses/rust"
            member = notices / "packages/example/LICENSE-MIT"
            member.parent.mkdir(parents=True)
            member.write_text("MIT example")
            catalogs = {}
            for name, relative in (("runtime", "runtime/manifest.json"),
                                   ("opencc", "opencc-manifest.json"),
                                   ("spelling", "spelling/manifest.json")):
                native = installed / relative
                native.parent.mkdir(parents=True, exist_ok=True)
                native.write_text("catalog-" + name)
                catalogs[name] = native
            sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
            package = lambda name, scope: {"name": name, "version": "1",
                       "source": "workspace", "crate_checksum": None,
                       "license_expression": "MIT", "selected_license": "MIT",
                       "scopes": [scope], "scope_features": {scope: []},
                       "notices": [{"path": "packages/example/LICENSE-MIT"}]}
            catalog = {"complete": True, "target": "x86_64-pc-windows-msvc",
                       "roots": ["desktop-app", "owned-daemon"],
                       "locks": {"desktop": "a" * 64, "owned-daemon": "b" * 64},
                       "packages": [package("voicetype-desktop", "desktop-app"),
                                    package("voicetyped", "owned-daemon")],
                       "files": [{"path": "packages/example/LICENSE-MIT", "bytes": member.stat().st_size,
                                  "sha256": sha(member)}],
                       "native_components": [{"name": name, "catalog_sha256": sha(path),
                                              "notice_count": 1} for name, path in catalogs.items()]}
            expected = root / "expected.json"
            expected.write_text(json.dumps(catalog))
            (notices / "manifest.json").write_bytes(expected.read_bytes())
            self.assertEqual(INSTALLED["verify"](installed, expected), (2, 1, 3))
            missing_runtime = dict(catalog, native_components=[item for item in catalog["native_components"]
                                                                if item["name"] != "runtime"])
            expected.write_text(json.dumps(missing_runtime))
            with self.assertRaisesRegex(AssertionError, "native notice catalogs"):
                INSTALLED["verify"](installed, expected)
            missing_daemon = dict(catalog, packages=[catalog["packages"][0]])
            expected.write_text(json.dumps(missing_daemon))
            (notices / "manifest.json").write_bytes(expected.read_bytes())
            with self.assertRaisesRegex(AssertionError, "daemon Rust graph"):
                INSTALLED["verify"](installed, expected)
            expected.write_text(json.dumps(catalog))
            (notices / "manifest.json").write_bytes(expected.read_bytes())
            (notices / "extra").write_text("uncataloged")
            with self.assertRaisesRegex(AssertionError, "member set"):
                INSTALLED["verify"](installed, expected)


if __name__ == "__main__":
    unittest.main()
