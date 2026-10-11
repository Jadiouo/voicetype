"""Fail-closed probes for the external Windows PE distribution inventory."""
import hashlib
import json
from pathlib import Path
import runpy
import tempfile
import unittest


SCRIPT = runpy.run_path(str(Path(__file__).resolve().parents[2] /
                            "scripts/catalog-windows-pe-distribution.py"))


class DistributionInventoryTests(unittest.TestCase):
    def test_shipped_notice_bytes_and_presence_must_match_scope_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            notice = root / "licenses/LICENSE.txt"
            notice.parent.mkdir()
            notice.write_bytes(b"original notice")
            manifest = {"files": [{"path": "licenses/LICENSE.txt", "bytes": notice.stat().st_size,
                                    "sha256": SCRIPT["sha256"](notice)}]}
            SCRIPT["verify_manifest_files"](manifest, root)
            notice.write_bytes(b"changed notice")
            with self.assertRaisesRegex(RuntimeError, "bytes differ"):
                SCRIPT["verify_manifest_files"](manifest, root)
            notice.unlink()
            with self.assertRaisesRegex(RuntimeError, "missing"):
                SCRIPT["verify_manifest_files"](manifest, root)

    def test_wrong_type_catalog_records_incomplete_without_blocking_ui(self):
        import os
        import subprocess
        import sys
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            extracted = root / "extracted"
            extracted.mkdir()
            installed = root / "installed"
            installed.mkdir()
            target = root / "target"
            (target / "installer-notices").mkdir(parents=True)
            (target / "installer-notices/plugin-origins.json").write_text("[]")
            (target / "installer-pe-audit.json").write_text("{}")
            setup = root / "setup.exe"
            body = bytearray(128)
            body[:2] = b"MZ"
            body[0x3c:0x40] = (64).to_bytes(4, "little")
            body[64:68] = b"PE\0\0"
            setup.write_bytes(body)
            output = root / "report.json"
            script = Path(__file__).resolve().parents[2] / "scripts/catalog-windows-pe-distribution.py"
            result = subprocess.run([sys.executable, str(script), str(extracted), str(installed),
                                     str(setup), str(target), str(output)],
                                    env={**os.environ, "GITHUB_SHA": "0" * 40},
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(output.read_text())
            self.assertIs(report["complete"], False)
            self.assertIn("TypeError", report["unresolved"][0])

    def test_unknown_missing_changed_and_case_colliding_pe_fail(self):
        rows = [{"path": "app.exe", "bytes": 4, "sha256": "a" * 64}]
        actual = {"app.exe": dict(rows[0])}
        SCRIPT["compare_inventory"](actual, rows)
        for altered, expected, reason in (
            ({**actual, "extra.dll": {"path": "extra.dll", "bytes": 1, "sha256": "b" * 64}},
             rows, "extra"),
            ({}, rows, "missing"),
            ({"app.exe": {"path": "app.exe", "bytes": 4, "sha256": "b" * 64}},
             rows, "bytes differ"),
            (actual, rows + [{"path": "APP.exe", "bytes": 4, "sha256": "a" * 64}],
             "collide"),
        ):
            with self.subTest(reason=reason), self.assertRaisesRegex(RuntimeError, reason):
                SCRIPT["compare_inventory"](altered, expected)

    def test_missing_csc_notice_never_counts_as_complete(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "spelling"
            provenance = root / "provenance"
            provenance.mkdir(parents=True)
            source = provenance / "windows-pe-sources.json"
            source.write_text("{}")
            pe = root / "_internal/native.pyd"
            pe.parent.mkdir()
            body = bytearray(128)
            body[:2] = b"MZ"
            body[0x3c:0x40] = (64).to_bytes(4, "little")
            body[64:68] = b"PE\0\0"
            pe.write_bytes(body)
            origin = {"complete": True, "msvc_redistribution_eligibility": "not_assessed",
                      "source_catalog_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
                      "binaries": [{"path": "_internal/native.pyd", "bytes": len(body),
                                    "sha256": hashlib.sha256(body).hexdigest(),
                                    "notice": "licenses/source.txt", "notice_sha256": "0" * 64}]}
            (provenance / "binary-origins.json").write_text(json.dumps(origin))
            with self.assertRaisesRegex(RuntimeError, "notice missing"):
                SCRIPT["check_csc_origins"](Path(directory))


if __name__ == "__main__":
    unittest.main()
