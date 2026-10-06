#!/usr/bin/env python3
"""Regression tests for meaning/offset preservation, independent of weights."""
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]/"scripts"))
from voicetype_csc import guard_edits, apply_edits


def proposal(text, source, target, **kwargs):
    return dict(start=text.index(source), source=source, target=target,
                probability=kwargs.get("probability", .999),
                original_probability=kwargs.get("original_probability", .0001))


class PolicyTests(unittest.TestCase):
    def test_repairs_unknown_homophone_and_keeps_unicode_offsets(self):
        text = "🎙️ GitHub 這次的結過很好，0.05 不要改。"
        edit = proposal(text, "過", "果")
        self.assertEqual(apply_edits(text, guard_edits(text, [edit])),
                         "🎙️ GitHub 這次的結果很好，0.05 不要改。")

    def test_rejects_real_semantic_regression_even_when_confident(self):
        text = "我說的是關閉自動送出，不是關閉錄音。"
        self.assertEqual(guard_edits(text, [proposal(text, "音", "影")]), [])

    def test_pronouns_and_buy_sell_meaning_are_not_inferred_from_stereotypes(self):
        for text, source, target in [("他做事一向很仔細。", "他", "她"),
                                     ("你是我的朋友。", "你", "妳"),
                                     ("我想買這個軟體。", "買", "賣"),
                                     ("我要賣掉機車。", "賣", "買")]:
            with self.subTest(text=text):
                self.assertEqual(guard_edits(text, [proposal(text, source, target)]), [])

    def test_negation_and_numbers_are_immutable(self):
        for text, source, target in [("不是這個", "不", "布"), ("尚未完成", "未", "味"),
                                     ("已經買了三個", "三", "山"), ("這是布料", "布", "不")]:
            with self.subTest(text=text):
                self.assertEqual(guard_edits(text, [proposal(text, source, target)]), [])

    def test_literals_names_and_canonical_terms_are_protected(self):
        examples = ["`結過`", "```text\n結過\n```", "``结過`剩餘過", "/tmp/結過.txt", "結過.txt",
                    "這個變數名稱是結過。", "「結過」", "林心華教授今天會來。"]
        for text in examples:
            source, target = ("心", "新") if "心" in text else ("過", "果")
            with self.subTest(text=text):
                self.assertEqual(guard_edits(text, [proposal(text, source, target)]), [])
        text = "星心科技正在招人。"
        self.assertEqual(guard_edits(text, [proposal(text, "心", "新")], ["星心科技"]), [])

    def test_only_the_literal_span_is_protected(self):
        text = "`新` 今天新情很好。"
        edit = dict(start=6, source="新", target="心", probability=.999, original_probability=.0001)
        self.assertEqual(apply_edits(text, guard_edits(text, [edit])), "`新` 今天心情很好。")

    def test_single_quotes_are_literals_but_english_apostrophes_are_not(self):
        for text in ["他說‘今天新情很好。’", "他說'今天新情很好。'", "'don't 改新情'", "‘don’t 改新情’"]:
            with self.subTest(text=text):
                self.assertEqual(guard_edits(text, [proposal(text, "新", "心")]), [])
        for text in ["It's 今天新情很好。", "James' 今天新情很好。", "don’t 今天新情很好。"]:
            with self.subTest(text=text):
                self.assertEqual(apply_edits(text, guard_edits(text, [proposal(text, "新", "心")])),
                                 text.replace("新", "心"))

    def test_low_or_invalid_confidence_preserves_original(self):
        text = "今天新情很好。"
        for probability in [.97, float("nan"), float("inf"), 1.1]:
            with self.subTest(probability=probability):
                self.assertEqual(guard_edits(text, [proposal(text, "新", "心", probability=probability)]), [])
        self.assertEqual(guard_edits(text, [proposal(text, "新", "心", original_probability=.2)]), [])

    def test_stale_positions_and_non_han_edits_are_rejected(self):
        text = "今天新情很好。"
        edit = proposal(text, "新", "心")
        edit["start"] = 0
        self.assertEqual(guard_edits(text, [edit]), [])
        self.assertEqual(guard_edits("cat", [proposal("cat", "a", "o")]), [])

    def test_multiple_real_typos_do_not_delete_repetition(self):
        text = "新情很好，結過很好。"
        edits = [proposal(text, "新", "心"), proposal(text, "過", "果")]
        self.assertEqual(apply_edits(text, guard_edits(text, edits)), "心情很好，結果很好。")


if __name__ == "__main__":
    unittest.main()
