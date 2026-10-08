"""Rust notice graph and installed-file boundary fixtures."""
import hashlib
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
            native = installed / "spelling/manifest.json"
            native.parent.mkdir()
            native.write_text("catalog")
            sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
            catalog = {"complete": True, "packages": [{"name": "example", "version": "1",
                       "source": "workspace", "crate_checksum": None,
                       "license_expression": "MIT", "selected_license": "MIT",
                       "scopes": ["desktop-app"], "scope_features": {"desktop-app": []},
                       "notices": [{"path": "packages/example/LICENSE-MIT"}]}],
                       "files": [{"path": "packages/example/LICENSE-MIT", "bytes": member.stat().st_size,
                                  "sha256": sha(member)}],
                       "native_components": [{"name": "spelling", "catalog_sha256": sha(native),
                                              "notice_count": 1}]}
            expected = root / "expected.json"
            expected.write_text(json.dumps(catalog))
            (notices / "manifest.json").write_bytes(expected.read_bytes())
            self.assertEqual(INSTALLED["verify"](installed, expected), (1, 1, 1))
            (notices / "extra").write_text("uncataloged")
            with self.assertRaisesRegex(AssertionError, "member set"):
                INSTALLED["verify"](installed, expected)


if __name__ == "__main__":
    unittest.main()
