"""Persistence/data-loss checks for the GUI's vocabulary boundary (CPU only)."""
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from voicetype_vocab import Vocabulary, validate


class VocabularyTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "vocab.toml"

    def load(self, text):
        self.path.write_text(text)
        return Vocabulary(self.path)

    def test_crud_preserves_unedited_fields_comments_and_unknown_keys(self):
        v = self.load('# keep this\nnames=["GitHub"]\nterms=["one"]\nfuture="kept"\n'
                      '[[entry]]\n# rule note\nwrong=["git hub"]\nright="GitHub"\nfuture_rule=3\n')
        v.put(0, ["git hub", "gthub"], "GitHub")
        v.put(None, ["錯字"], "正字")
        v.delete(1)
        v.set_names(["GitHub", "台積電"])
        result = self.path.read_text()
        self.assertIn('# keep this', result)
        self.assertIn('# rule note', result)
        self.assertEqual(validate(result.encode())["future"], "kept")
        self.assertEqual(Vocabulary(self.path).entries[0]["future_rule"], 3)
        self.assertEqual(Vocabulary(self.path).names, ["GitHub", "台積電"])

    def test_missing_file_create_and_restore_exact_previous_bytes(self):
        v = Vocabulary(self.path)
        v.put(None, ["mistype"], "correct")
        before = self.path.read_bytes()
        v.put(0, ["mistype"], "replacement")
        self.assertEqual(v.backup.read_bytes(), before)
        v.restore()
        self.assertEqual(self.path.read_bytes(), before)
        self.assertEqual(v.backup.stat().st_mode & 0o777, 0o600)

    def test_empty_and_inline_entry_arrays_support_editing(self):
        for original in ['entry=[]\n', 'entry=[{wrong=["sample"],right="Original"}]\n']:
            with self.subTest(original=original):
                v = self.load(original)
                v.put(None, ['sample2'], 'Example')
                v.set_names(['Example'])
                v.put(len(v.entries) - 1, ['sample3'], 'Changed')
                self.assertEqual(Vocabulary(self.path).entries[-1]['right'], 'Changed')
                while v.entries:
                    v.delete(0)
                v.put(None, ['sample4'], 'Final')
                self.assertEqual(Vocabulary(self.path).entries[0]['right'], 'Final')

    def test_case_insensitive_conflict_leaves_file_and_model_unchanged(self):
        v = self.load('[[entry]]\nwrong=["sample"]\nright="One"\n')
        original = self.path.read_bytes()
        with self.assertRaisesRegex(ValueError, '已對應'):
            v.put(None, ["SAMPLE"], "Two")
        self.assertEqual(self.path.read_bytes(), original)
        self.assertEqual(len(v.entries), 1)

    def test_opencc_derived_alias_conflict_is_rejected_before_save(self):
        v = self.load('names=["台積電"]\n')
        with self.assertRaisesRegex(ValueError, '已對應'):
            v.put(None, ["臺積電"], "Other")
        self.assertEqual(self.path.read_text(), 'names=["台積電"]\n')

    def test_external_update_and_delete_are_not_overwritten(self):
        v = self.load('names=["GitHub"]\n')
        self.path.write_text('names=["NewName"]\n')
        with self.assertRaisesRegex(ValueError, '其他地方'):
            v.put(None, ["typo"], "term")
        self.assertEqual(self.path.read_text(), 'names=["NewName"]\n')
        self.path.unlink()
        with self.assertRaisesRegex(ValueError, '其他地方'):
            v.set_names(["Other"])
        self.assertFalse(self.path.exists())

    def test_conflicting_derived_name_aliases_are_rejected(self):
        with self.assertRaisesRegex(ValueError, '大小寫衝突'):
            validate('names=["台ABC", "台abc"]'.encode())

    def test_invalid_initial_file_is_not_replaced(self):
        self.path.write_text('invalid = [')
        with self.assertRaises(ValueError):
            Vocabulary(self.path)
        self.assertEqual(self.path.read_text(), 'invalid = [')

    def test_failed_replace_preserves_previous_file_and_cleans_temp(self):
        v = self.load('names=["GitHub"]\n')
        original = self.path.read_bytes()
        replace = os.replace
        def fail_destination(source, destination):
            if Path(destination) == self.path:
                raise OSError('simulated full filesystem')
            return replace(source, destination)
        with patch('voicetype_vocab.os.replace', side_effect=fail_destination):
            with self.assertRaises(OSError):
                v.put(None, ["mistype"], "correct")
        self.assertEqual(self.path.read_bytes(), original)
        self.assertEqual(v.original, original)
        self.assertEqual(list(self.path.parent.glob('.vocab.toml-*')), [])

    def test_limits_types_and_control_characters(self):
        for source in [b'names=["X"]', b'entry="wrong type"', b'[[entry]]\nwrong=[]\nright="X"',
                       b'[[entry]]\nwrong=["sample"]\nright=""',
                       b'[[entry]]\nwrong=["a\\nb"]\nright="X"', b'names=[1]',
                       b'#' * (128 * 1024 + 1)]:
            with self.subTest(source=source[:70]):
                with self.assertRaises(ValueError):
                    validate(source)


if __name__ == '__main__':
    unittest.main()
