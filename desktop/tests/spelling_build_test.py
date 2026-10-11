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
    def test_numpy_msvc_copies_have_exact_official_runtime_source(self):
        catalog, records = BUILDER["windows_pe_source_records"]()
        official = catalog["msvc_numpy_runtime"]
        self.assertEqual(set(official["paths"]), {
            "_internal/msvcp140.dll",
            "_internal/numpy.libs/msvcp140-a4c2229bdc2a2a630acdc095b4d86008.dll",
        })
        self.assertEqual(official["source_member"], "msvcp140.dll_amd64")
        self.assertEqual(official["notice_source_member"], "u4")
        for path in official["paths"]:
            self.assertEqual(records[path]["source_member_sha256"], official["sha256"])
            self.assertEqual(records[path]["microsoft_runtime_source"]["notice_sha256"],
                             official["notice_sha256"])
        with tempfile.TemporaryDirectory() as directory:
            damaged = Path(directory) / "sources.json"
            catalog["msvc_numpy_runtime"]["sha256"] = "0" * 64
            damaged.write_text(json.dumps(catalog))
            with mock.patch.dict(BUILDER["windows_pe_source_records"].__globals__,
                                 {"WINDOWS_PE_SOURCES": damaged}):
                with self.assertRaisesRegex(RuntimeError, "NumPy Microsoft runtime differs"):
                    BUILDER["windows_pe_source_records"]()

    def test_generated_windows_executable_records_pinned_bootloader_and_recipe(self):
        def pe(text, resource):
            data = bytearray(0x400)
            data[:2] = b"MZ"
            data[0x3c:0x40] = (0x80).to_bytes(4, "little")
            data[0x80:0x84] = b"PE\0\0"
            data[0x86:0x88] = (2).to_bytes(2, "little")
            data[0x94:0x96] = (0xe0).to_bytes(2, "little")
            table = 0x80 + 24 + 0xe0
            for index, (name, value, offset) in enumerate(((b".text", text, 0x200),
                                                           (b".rsrc", resource, 0x300))):
                section = table + index * 40
                data[section:section + len(name)] = name
                data[section + 16:section + 20] = len(value).to_bytes(4, "little")
                data[section + 20:section + 24] = offset.to_bytes(4, "little")
                data[offset:offset + len(value)] = value
            return bytes(data)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "run.exe"
            output = root / "voicetype-csc.exe"
            spec = root / "voicetype-csc.spec"
            entry = root / "voicetype_csc.py"
            source.write_bytes(pe(b"source-code", b"old-resource"))
            output.write_bytes(pe(b"source-code", b"new-resource"))
            spec.write_text("generated spec")
            entry.write_text("app entry")
            record = BUILDER["generated_windows_executable_origin"](
                output, source, spec, entry, ["--onedir", "--noupx"],
                expected_bootloader_sha256=hashlib.sha256(source.read_bytes()).hexdigest())
            self.assertEqual(record["path"], "voicetype-csc.exe")
            self.assertEqual(record["source_member"], "PyInstaller/bootloader/Windows-64bit-intel/run.exe")
            self.assertEqual(record["bootloader_sha256"], hashlib.sha256(source.read_bytes()).hexdigest())
            self.assertEqual(record["entry_sha256"], hashlib.sha256(entry.read_bytes()).hexdigest())
            self.assertEqual(record["spec_sha256"], hashlib.sha256(spec.read_bytes()).hexdigest())
            self.assertEqual(record["unchanged_bootloader_sections"], [".text"])
            self.assertEqual(record["notice"], "licenses/pyinstaller/00-COPYING.txt")
            changed = bytearray(output.read_bytes())
            changed[0x200] ^= 1
            output.write_bytes(changed)
            with self.assertRaisesRegex(RuntimeError, "bootloader code section"):
                BUILDER["generated_windows_executable_origin"](
                    output, source, spec, entry, ["--onedir", "--noupx"],
                    expected_bootloader_sha256=hashlib.sha256(source.read_bytes()).hexdigest())

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

    def test_windows_complete_requires_every_source_pe_and_generated_executable(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory)
            source = bundle / "_internal/source.pyd"
            source.parent.mkdir()
            generated = bundle / "voicetype-csc.exe"
            data = bytearray(128)
            data[:2] = b"MZ"
            data[0x3c:0x40] = (64).to_bytes(4, "little")
            data[64:68] = b"PE\0\0"
            source.write_bytes(data)
            generated.write_bytes(data)
            notice = bundle / "licenses/source.txt"
            notice.parent.mkdir()
            notice.write_text("synthetic notice")
            checksum = hashlib.sha256(data).hexdigest()
            origin = {"source_artifact": "fixture archive", "source_sha256": "fixture",
                      "source_member_sha256": checksum, "notice": "licenses/source.txt",
                      "notice_sha256": hashlib.sha256(notice.read_bytes()).hexdigest()}
            generated_origin = {"path": generated.name, "bytes": len(data), "sha256": checksum,
                                "source_artifact": "fixture wheel", "source_sha256": "fixture",
                                "notice": "licenses/source.txt"}
            with mock.patch.object(sys, "platform", "win32"), mock.patch.dict(
                    BUILDER["binary_origins"].__globals__,
                    {"windows_pe_source_records": lambda: ({}, {"_internal/source.pyd": origin})}):
                complete = BUILDER["binary_origins"](bundle, generated_windows_executable=generated_origin)
                self.assertTrue(complete["complete"])
                self.assertEqual(complete["source_catalog_missing_paths"], [])
                source.unlink()
                missing = BUILDER["binary_origins"](bundle, generated_windows_executable=generated_origin)
                self.assertFalse(missing["complete"])
                self.assertEqual(missing["source_catalog_missing_paths"], ["_internal/source.pyd"])
                source.write_bytes(data)
                (bundle / "extra.dat").write_bytes(data)
                extra = BUILDER["binary_origins"](bundle, generated_windows_executable=generated_origin)
                self.assertFalse(extra["complete"])
                self.assertEqual(extra["source_catalog_pending_paths"], ["extra.dat"])

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
