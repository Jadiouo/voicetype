"""Focused provenance checks for the CPU spelling bundle builder."""
import runpy
from pathlib import Path
from unittest import mock
import tempfile
import sys
import sysconfig
import unittest


ROOT = Path(__file__).resolve().parents[2]
BUILDER = runpy.run_path(str(ROOT / "scripts/build-desktop-spelling.py"))


class SpellingBuildTests(unittest.TestCase):
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
