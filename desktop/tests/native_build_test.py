"""Installer build entry point: reject damaged inputs before publishing output."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class NativeBuildTests(unittest.TestCase):
    def test_corrupt_source_cache_leaves_destination_absent(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cache = root / "cache"
            cache.mkdir()
            (cache / "sherpa-onnx.archive").write_bytes(b"damaged download")
            result = subprocess.run(
                [sys.executable, str(ROOT / "scripts/build-desktop-native.py"),
                 "--cache", str(cache), "--output", str(root / "native"), "--offline"],
                capture_output=True, text=True, timeout=10,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("SHA256/size mismatch: sherpa-onnx", result.stderr)
            self.assertFalse((root / "native").exists())

    def test_existing_output_is_preserved_without_fetching(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "native"
            output.mkdir()
            (output / "keep").write_text("existing runtime")
            result = subprocess.run(
                [sys.executable, str(ROOT / "scripts/build-desktop-native.py"),
                 "--cache", str(root / "cache"), "--output", str(output), "--offline"],
                capture_output=True, text=True, timeout=10,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Output already exists", result.stderr)
            self.assertEqual((output / "keep").read_text(), "existing runtime")
            self.assertFalse((root / "cache").exists())


if __name__ == "__main__":
    unittest.main()
