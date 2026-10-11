"""Installer-only PE inventory must reject unknown or changed NSIS plugins."""
import hashlib
from pathlib import Path
import runpy
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
CATALOG = runpy.run_path(str(ROOT / "scripts/catalog-windows-installer-pe.py"))


def fake_pe(marker):
    data = bytearray(128)
    data[:2] = b"MZ"
    data[0x3c:0x40] = (64).to_bytes(4, "little")
    data[64:68] = b"PE\0\0"
    data[68:] = marker
    return bytes(data)


class WindowsInstallerPeTests(unittest.TestCase):
    def test_installed_plugin_notices_require_catalog_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            prepared = root / "prepared"
            installed = root / "install/licenses/installer"
            prepared.mkdir()
            installed.mkdir(parents=True)
            for name in ("plugin-origins.json", "NSIS-COPYING.txt", "NSISdl-License.txt",
                         "nsis-tauri-utils-LICENSE_MIT.txt",
                         "nsis-tauri-utils-LICENSE_APACHE-2.0.txt"):
                (prepared / name).write_text("fixture " + name)
                (installed / name).write_bytes((prepared / name).read_bytes())
            self.assertEqual(CATALOG["verify_installed_notices"](root / "install", prepared), 5)
            (installed / "NSISdl-License.txt").unlink()
            with self.assertRaisesRegex(RuntimeError, "installer notice"):
                CATALOG["verify_installed_notices"](root / "install", prepared)

    def test_source_cache_rejects_changed_archive(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            (cache / "nsis-3.11.zip").write_bytes(b"different archive")
            with self.assertRaisesRegex(RuntimeError, "source archive SHA-256"):
                CATALOG["pinned_source"](cache, "nsis-3.11.zip", "unused URL", "0" * 64)

    def test_plugin_inventory_requires_exact_magic_set_and_source_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            plugins = Path(directory) / "$PLUGINSDIR"
            plugins.mkdir()
            records = []
            for name in ("LangDLL.dll", "NSISdl.dll", "StartMenu.dll", "System.dll",
                         "nsDialogs.dll", "nsis_tauri_utils.dll"):
                payload = fake_pe(name.encode())
                (plugins / name).write_bytes(payload)
                records.append({"path": f"$PLUGINSDIR/{name}",
                                "bytes": len(payload),
                                "sha256": hashlib.sha256(payload).hexdigest()})
            observed = CATALOG["verify_plugin_inventory"](Path(directory), records)
            self.assertEqual(len(observed), 6)
            (plugins / "NSISdl.dll").write_bytes(fake_pe(b"changed"))
            with self.assertRaisesRegex(RuntimeError, "plugin bytes"):
                CATALOG["verify_plugin_inventory"](Path(directory), records)
            (plugins / "NSISdl.dll").write_bytes(fake_pe(b"NSISdl.dll"))
            (plugins / "unexpected.bin").write_bytes(fake_pe(b"extra PE"))
            with self.assertRaisesRegex(RuntimeError, "plugin PE set"):
                CATALOG["verify_plugin_inventory"](Path(directory), records)


if __name__ == "__main__":
    unittest.main()
