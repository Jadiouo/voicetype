"""Focused isolation regressions for the two-real-NSIS upgrade probe."""
import runpy
from pathlib import Path
import stat
import tempfile
import unittest
from unittest import mock


SMOKE = runpy.run_path(str(Path(__file__).with_name("windows_upgrade_smoke.py")))


class UpgradeIsolationTests(unittest.TestCase):
    def test_only_requested_or_direct_ebwebview_profile_is_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            requested = Path(directory) / "webview2"
            child = requested / "EBWebView"
            child.mkdir(parents=True)
            sibling = requested / "OtherWebView"
            sibling.mkdir()
            verify = SMOKE["expected_webview_profile"]
            self.assertTrue(verify(requested, requested))
            self.assertTrue(verify(child, requested))
            self.assertFalse(verify(sibling, requested))
            self.assertFalse(verify(requested / "EBWebView" / "..", requested))
            with mock.patch.object(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400, create=True):
                original = Path.lstat
                def reparse(path):
                    details = original(path)
                    if path == child:
                        return mock.Mock(st_mode=details.st_mode, st_file_attributes=0x400)
                    return details
                with mock.patch.object(Path, "lstat", reparse):
                    self.assertFalse(verify(child, requested))

    def test_only_new_empty_framework_directory_can_be_observed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scratch = root / "scratch"
            scratch.mkdir()
            local = root / "LocalAppData"
            local.mkdir()
            product = local / "io.github.jadiouo.voicetype.preview"
            key = "local/io.github.jadiouo.voicetype.preview"
            before = {key: False}
            def current(_env, _scratch):
                return {key: product.exists()}
            with mock.patch.dict(SMOKE["assert_known_folders_unchanged"].__globals__,
                                 {"known_folder_product_paths": current,
                                  "powershell_json": lambda *_args: {"local": str(local)}}):
                product.mkdir()
                report = {}
                SMOKE["assert_known_folders_unchanged"]({}, scratch, before, report, "old", True)
                self.assertTrue(product.is_dir())
                self.assertEqual(report["framework_empty_known_folder_observed"],
                                 [{"phase": "old", "path": key}])
                (product / "content").write_text("synthetic")
                with self.assertRaisesRegex(RuntimeError, "product data appeared"):
                    SMOKE["assert_known_folders_unchanged"]({}, scratch, before, {}, "old", True)
                self.assertTrue((product / "content").exists())
                (product / "content").unlink()
                with self.assertRaisesRegex(RuntimeError, "product data appeared"):
                    SMOKE["assert_known_folders_unchanged"]({}, scratch, before, {}, "old", False)
                product.rmdir()
            with self.assertRaisesRegex(RuntimeError, "already exists"):
                SMOKE["assert_clean_known_folder_baseline"]({key: True})

    def test_reparse_and_other_known_folder_cannot_use_empty_exception(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            product = root / "io.github.jadiouo.voicetype.preview"
            product.mkdir()
            with mock.patch.object(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400, create=True):
                with mock.patch.object(Path, "lstat", return_value=mock.Mock(
                        st_mode=stat.S_IFDIR, st_file_attributes=0x400)):
                    self.assertFalse(SMOKE["is_empty_framework_directory"](product))
            self.assertTrue(product.exists())
            before = {"local/other": False}
            with mock.patch.dict(SMOKE["assert_known_folders_unchanged"].__globals__,
                                 {"known_folder_product_paths": lambda *_: {"local/other": True},
                                  "powershell_json": lambda *_: {"local": str(root),
                                                                   "roaming": str(root)}}):
                with self.assertRaisesRegex(RuntimeError, "product data appeared"):
                    SMOKE["assert_known_folders_unchanged"]({}, root / "scratch", before, {}, "old", True)


if __name__ == "__main__":
    unittest.main()
