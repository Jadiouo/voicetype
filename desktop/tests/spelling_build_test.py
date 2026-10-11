"""Focused provenance checks for the CPU spelling bundle builder."""
import runpy
from pathlib import Path
from unittest import mock
import tempfile
import sys
import sysconfig
import unittest
import hashlib
import json
import runpy


ROOT = Path(__file__).resolve().parents[2]
BUILDER = runpy.run_path(str(ROOT / "scripts/build-desktop-spelling.py"))
INSTALLED = runpy.run_path(str(ROOT / "desktop/tests/installed_spelling.py"))


class SpellingBuildTests(unittest.TestCase):
    def test_windows_bundle_omits_only_os_ucrt_forwarders(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory)
            internal = bundle / "_internal"
            internal.mkdir()
            for name in ("api-ms-win-crt-runtime-l1-1-0.dll",
                         "api-ms-win-core-file-l1-1-0.dll", "ucrtbase.dll",
                         "MSVCP140_1.dll", "vcruntime140.dll", "python312.dll"):
                data = bytearray(128)
                data[:2] = b"MZ"
                data[0x3c:0x40] = (64).to_bytes(4, "little")
                data[64:68] = b"PE\0\0"
                (internal / name).write_bytes(data)
            nested = internal / "onnxruntime/capi/api-ms-win-crt-time-l1-1-0.dll"
            nested.parent.mkdir(parents=True)
            nested.write_bytes(data)
            excluded = BUILDER["remove_windows_os_ucrt"](bundle)
            self.assertEqual({path.name for path in internal.rglob("*") if path.is_file()},
                             {"MSVCP140_1.dll", "vcruntime140.dll", "python312.dll"})
            self.assertEqual({entry["path"] for entry in excluded},
                             {"_internal/api-ms-win-crt-runtime-l1-1-0.dll",
                              "_internal/api-ms-win-core-file-l1-1-0.dll", "_internal/ucrtbase.dll",
                              "_internal/onnxruntime/capi/api-ms-win-crt-time-l1-1-0.dll"})
            self.assertTrue(all(entry["bytes"] == 128 and
                                entry["sha256"] == hashlib.sha256(data).hexdigest()
                                for entry in excluded))
            (internal / "api-ms-win-unreviewed.dll").write_bytes(data)
            with self.assertRaisesRegex(RuntimeError, "unreviewed Windows API-set"):
                BUILDER["remove_windows_os_ucrt"](bundle)

    def test_windows_pe_inventory_includes_frozen_executable_by_magic(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory)
            for name in ("voicetype-csc.exe", "_internal/native.pyd", "_internal/other.bin"):
                path = bundle / name
                path.parent.mkdir(parents=True, exist_ok=True)
                data = bytearray(128)
                data[:2] = b"MZ"
                data[0x3c:0x40] = (64).to_bytes(4, "little")
                data[64:68] = b"PE\0\0"
                path.write_bytes(data)
            with mock.patch.object(sys, "platform", "win32"):
                origins = BUILDER["binary_origins"](bundle)
            self.assertFalse(origins["complete"])
            self.assertEqual({entry["path"] for entry in origins["binaries"]},
                             {"voicetype-csc.exe", "_internal/native.pyd", "_internal/other.bin"})
            for entry in origins["binaries"]:
                self.assertEqual(entry["sha256"], hashlib.sha256((bundle / entry["path"]).read_bytes()).hexdigest())
                self.assertEqual(entry["bytes"], 128)

    def test_installed_windows_pe_set_rejects_missing_executable_origin(self):
        with tempfile.TemporaryDirectory() as directory:
            install = Path(directory) / "install"
            bundle = install / "spelling"
            bundle.mkdir(parents=True)
            pe = bytearray(128)
            pe[:2] = b"MZ"
            pe[0x3c:0x40] = (64).to_bytes(4, "little")
            pe[64:68] = b"PE\0\0"
            (bundle / "voicetype-csc.exe").write_bytes(pe)
            (bundle / "provenance").mkdir()
            (bundle / "provenance/binary-origins.json").write_text(
                json.dumps({"complete": False, "binaries": []}))
            for name in ("models/model-int8-fused.onnx", "models/tokenizer.json",
                         "licenses/MacBERT-Apache-2.0.txt"):
                path = bundle / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("fixture")
            files = [{"path": path.relative_to(bundle).as_posix(),
                      "bytes": path.stat().st_size,
                      "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
                     for path in bundle.rglob("*") if path.is_file()]
            expected = Path(directory) / "expected.json"
            expected.write_text(json.dumps({"platform": "windows-x86_64", "files": files}))
            (bundle / "manifest.json").write_bytes(expected.read_bytes())
            with self.assertRaisesRegex(AssertionError, "PE.*origin|origin.*PE"):
                INSTALLED["verify"](install, expected)

    @unittest.skipUnless(sys.platform == "linux", "Linux provenance fixture")
    def test_hosted_python_runtime_is_matched_by_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory) / "hosted-python"
            name = sysconfig.get_config_var("INSTSONAME")
            library = prefix / "lib" / name
            library.parent.mkdir(parents=True)
            library.write_bytes(b"hosted Python runtime")
            copied = Path(directory) / "_internal" / name
            copied.parent.mkdir()
            copied.write_bytes(library.read_bytes())

            self.assertEqual(BUILDER["python_runtime_source"](copied, prefix), library)
            copied.write_bytes(b"different system Python runtime")
            self.assertIsNone(BUILDER["python_runtime_source"](copied, prefix))

    @unittest.skipUnless(sys.platform == "linux", "Linux provenance fixture")
    def test_hosted_python_library_uses_python_notice(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            prefix = root / "hosted-python"
            name = sysconfig.get_config_var("INSTSONAME")
            library = prefix / "lib" / name
            library.parent.mkdir(parents=True)
            library.write_bytes(b"hosted Python runtime")
            bundle = root / "bundle"
            copied = bundle / "_internal" / name
            copied.parent.mkdir(parents=True)
            copied.write_bytes(library.read_bytes())
            (bundle / "licenses/pyinstaller").mkdir(parents=True)
            (bundle / "licenses/Python.txt").write_text("Python license")
            (bundle / "licenses/pyinstaller/LICENSE").write_text("PyInstaller license")

            with mock.patch.object(sys, "base_prefix", str(prefix)):
                origins = BUILDER["binary_origins"](bundle)
            python = next(entry for entry in origins["binaries"] if entry["path"] == f"_internal/{name}")
            self.assertEqual(python["origin"], "Python")
            self.assertEqual(python["notice"], "licenses/Python.txt")
            self.assertEqual(python["source"], str(library))
            copied.write_bytes(b"same basename, different binary")
            with mock.patch.object(sys, "base_prefix", str(prefix)):
                with self.assertRaisesRegex(RuntimeError, "unknown copied system library"):
                    BUILDER["binary_origins"](bundle)


if __name__ == "__main__":
    unittest.main()
