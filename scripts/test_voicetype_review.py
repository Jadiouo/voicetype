import json
from pathlib import Path
import tempfile
import time
import unittest

from voicetype_review import ReviewStore, RETENTION, propose_pair, json_bytes


class ReviewTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / 'review'
        self.root.mkdir()
        self.vocab = Path(self.temp.name) / 'vocab.toml'
        self.store = ReviewStore(self.root)

    def sample(self, text, sequence=1, **updates):
        item_id = f'r-{int(time.time())}-1-{sequence}'
        path = self.root / item_id; path.mkdir()
        value = dict(version=1, id=item_id, created_at=int(time.time()), duration_ms=12000,
                     sample_rate=16000, asr_text=text, output_text=text, status='pending',
                     corrected_text=None, reviewed_at=None)
        value.update(updates)
        (path/'record.json').write_bytes(json_bytes(value))
        (path/'audio.wav').write_bytes(b'audio fixture')
        return value

    def test_correct_only_records_confirmation_without_vocabulary(self):
        r = self.sample('這句正確，保留 GitHub。')
        self.store.review(r['id'], r['output_text'], expected_output=r['output_text'])
        after = self.store.list()[0]
        self.assertEqual(after['status'], 'correct')
        self.assertFalse(self.vocab.exists())

    def test_sentence_correction_retains_full_text_without_learning(self):
        r = self.sample('a very wrong sentence')
        self.store.review(r['id'], '整句修改，留下新的完整句子。', expected_output=r['output_text'])
        self.assertEqual(self.store.list()[0]['corrected_text'], '整句修改，留下新的完整句子。')
        self.assertFalse(self.vocab.exists())

    def test_explicit_term_promotion_is_idempotent(self):
        r = self.sample('推到 GTHUB。')
        args = dict(expected_output=r['output_text'], learn=True, vocabulary_path=self.vocab)
        self.assertEqual(self.store.review(r['id'], '推到 GitHub。', **args), ('GTHUB', 'GitHub'))
        original = self.vocab.read_bytes()
        self.store.review(r['id'], '推到 GitHub。', **args)
        self.assertEqual(self.vocab.read_bytes(), original)
        self.assertEqual(self.store.list()[0]['promotion']['state'], 'applied')

    def test_known_correct_usage_blocks_new_rule(self):
        self.sample('GTHUB 是正確的名稱。', status='correct', corrected_text='GTHUB 是正確的名稱。')
        r = self.sample('推到 GTHUB。', sequence=2)
        with self.assertRaisesRegex(ValueError, '已確認'):
            self.store.review(r['id'], '推到 GitHub。', expected_output=r['output_text'], learn=True, vocabulary_path=self.vocab)
        self.assertFalse(self.vocab.exists())

    def test_dictionary_conflict_keeps_pending_intent_and_original_rule(self):
        self.vocab.write_text('[[entry]]\nwrong=["GTHUB"]\nright="ExistingName"\n')
        original = self.vocab.read_bytes()
        r = self.sample('推到 GTHUB。')
        with self.assertRaises(ValueError):
            self.store.review(r['id'], '推到 GitHub。', expected_output=r['output_text'], learn=True, vocabulary_path=self.vocab)
        after = self.store.list()[0]
        self.assertEqual(after['status'], 'pending')
        self.assertEqual(after['promotion']['state'], 'pending')
        self.assertEqual(after['pending_corrected_text'], '推到 GitHub。')
        self.assertEqual(self.vocab.read_bytes(), original)

    def test_expiration_removes_pending_and_reviewed_audio(self):
        a = self.sample('pending', created_at=int(time.time())-RETENTION-1)
        b = self.sample('reviewed', sequence=2, created_at=int(time.time())-RETENTION-1, status='correct')
        self.assertEqual(self.store.list(), [])
        self.assertFalse((self.root/a['id']).exists())
        self.assertFalse((self.root/b['id']).exists())

    def test_traversal_and_links_are_not_used_for_playback_or_deletion(self):
        with self.assertRaises(ValueError): self.store.delete('../outside')
        r = self.sample('sample')
        audio = self.root/r['id']/'audio.wav';audio.unlink();audio.symlink_to('/etc/passwd')
        with self.assertRaises(ValueError):self.store.audio_path(r['id'])

    def test_limited_suggestions_keep_english_context_and_abstain_on_rewrite(self):
        pair = propose_pair('先把修改 coming，再 push。', '先把修改 commit，再 push。')
        self.assertIsNotNone(pair)
        self.assertIn('修改 coming', pair[0])
        self.assertIsNone(propose_pair('coming', 'commit'))
        self.assertIsNone(propose_pair('alpha and beta', 'one and two'))
        self.assertIsNone(propose_pair('`GTHUB`', '`GitHub`'))
        self.assertIsNone(propose_pair('句子一樣', '句子一樣'))

    def test_malformed_optional_fields_are_skipped_without_breaking_queue(self):
        self.sample('bad', corrected_text={'unexpected': 'object'})
        self.sample('bad time', sequence=2, created_at=10**30)
        self.sample('bad audio', sequence=3, duration_ms='long')
        valid = self.sample('still readable', sequence=4)
        self.assertEqual([r['id'] for r in self.store.list()], [valid['id']])


if __name__ == '__main__':
    unittest.main()
