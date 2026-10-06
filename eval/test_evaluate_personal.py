"""Invariant tests for scoring/attribution, without microphone or ASR downloads."""
import argparse
import json
import os
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch
import wave

import evaluate_personal as ep


def case(reference="台大和台大", terms=None, category="test"):
    return {"id": "p001", "split": "dev", "category": category, "reference": reference,
            "terms": terms or [], "context": "SECRET CONTEXT", "notes": "SECRET NOTES"}


class ScoringTests(unittest.TestCase):
    def test_term_boundaries_aliases_and_counts(self):
        term = {"text": "maybe", "aliases": ["MAYBE"]}
        self.assertEqual(ep.term_count("Maybelline xmaybe maybe_2 maybe是 maybe!", term), 2)
        self.assertEqual(ep.term_count("Nav2 跟 Nav 2", {"text": "Nav2", "aliases": ["Nav 2"]}), 2)
        self.assertEqual(ep.term_count("sim-to-real", {"text": "sim to real", "aliases": ["sim-to-real"]}), 1)

    def test_duplicate_term_requires_duplicate_evidence(self):
        row = ep.score_case(case(terms=[{"text": "台大", "aliases": ["臺大"]}]), "台大和太大")
        self.assertEqual(row["terms"][0]["expected_occurrences"], 2)
        self.assertEqual(row["terms"][0]["matched_occurrences"], 1)
        self.assertEqual(ep.aggregate([row])["target_term_occurrence_recall_percent"], 50)

    def test_name_overproduction_cannot_credit_other_name(self):
        row = ep.score_case(case("林志豪和林智豪", [{"text": "林志豪"}, {"text": "林智豪"}]), "林智豪和林智豪")
        self.assertEqual(ep.aggregate([row])["target_term_occurrence_recall_percent"], 50)

    def test_weighted_cer_not_average_of_sentence_percentages(self):
        rows = [ep.score_case(case("甲"), "乙"), ep.score_case(case("甲乙丙丁戊己庚辛壬"), "甲乙丙丁戊己庚辛壬")]
        self.assertEqual(ep.aggregate(rows)["cer_percent"], 10)

    def test_english_wer_only_and_script_review(self):
        english = ep.score_case(case("Please push now.", category="english"), "Please push")
        mixed = ep.score_case(case("請 push"), "請プッシュ")
        result = ep.aggregate([english, mixed])
        self.assertEqual(result["english_reference_words"], 3)
        self.assertEqual(result["english_word_errors"], 1)
        self.assertTrue(mixed["unexpected_script_review"])
        self.assertEqual(mixed["pinyin_review"], "manual_required")

    def test_raw_tag_and_numeric_difference_not_hidden(self):
        row = ep.score_case(case("三個"), "<|ja|><|Speech|>3個")
        self.assertEqual(row["text"], "3個")
        self.assertEqual(row["unexpected_language_tags"], ["ja"])
        self.assertTrue(row["numeric_representation_review"])
        self.assertGreater(row["character_errors"], 0)


class AdapterTests(unittest.TestCase):
    def test_cpu_only_no_answers_and_no_ambient_context(self):
        with patch.dict(os.environ, {"VOICETYPE_CONTEXT_HELPER": "SECRET CONTEXT", "VOICETYPE_REFINER_URL": "SECRET"}):
            cmd, env = ep.engine_command("whisper", "/engine", "/model", "/audio.wav")
        self.assertIn("-ng", cmd)
        self.assertNotIn("--prompt", cmd)
        self.assertNotIn("VOICETYPE_CONTEXT_HELPER", env)
        self.assertEqual(env["CUDA_VISIBLE_DEVICES"], "-1")
        self.assertEqual(env["VK_DRIVER_FILES"], "/dev/null")

    def test_unsupported_device_and_threads_fail_explicitly(self):
        with self.assertRaisesRegex(ValueError, "AMD Vulkan"):
            ep.engine_command("whisper", "/engine", "/model", "/audio", device="amd-vulkan")
        with self.assertRaisesRegex(ValueError, "fixes inference threads"):
            ep.engine_command("daemon", "/engine", "/model", "/audio", threads=8)

    def test_runtime_failure_is_not_empty_success(self):
        with self.assertRaisesRegex(RuntimeError, "exited"):
            ep.run_engine(["/bin/false"], dict(os.environ), timeout=3)
        output = ep.run_engine(["/bin/echo", "text"], dict(os.environ), timeout=3)
        self.assertEqual(output["raw_output"], "text")
        self.assertGreater(output["elapsed_process_seconds"], 0)
        if ep.TIME_BIN.is_file():
            self.assertIsInstance(output["peak_rss_kib"], int)


class RecordingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.corpus = self.root / "manifest.json"
        self.corpus.write_text('{"frozen": true}')
        self.recordings = self.root / "recordings"
        self.recordings.mkdir()
        (self.recordings / "corpus.json").write_bytes(self.corpus.read_bytes())
        self.case = case()
        self.wav = self.recordings / "p001.wav"
        with wave.open(str(self.wav), "wb") as wav:
            wav.setnchannels(1)
            wav.setsampwidth(2)
            wav.setframerate(16000)
            wav.writeframes(struct.pack("<h", 3000) * 16000)
        self.meta = {"id": "p001", "confirmed_reading": True, "reference": self.case["reference"],
                     "corpus_sha256": ep.digest(self.corpus), "wav_sha256": ep.digest(self.wav)}
        self.save_meta()

    def tearDown(self):
        self.temp.cleanup()

    def save_meta(self):
        (self.recordings / "p001.json").write_text(json.dumps(self.meta))

    def validate(self):
        return ep.validate_recordings([self.case], self.corpus, self.recordings)

    def test_verified_then_tampered(self):
        self.assertEqual(len(self.validate()[0]), 1)
        self.wav.write_bytes(self.wav.read_bytes() + b"changed")
        self.assertIn("hash mismatch", self.validate()[1][0]["error"])

    def test_unconfirmed_or_reference_changed(self):
        self.meta["confirmed_reading"] = False
        self.save_meta()
        self.assertIn("not explicitly confirmed", self.validate()[1][0]["error"])
        self.meta["confirmed_reading"] = True
        self.meta["reference"] = "別的句子"
        self.save_meta()
        self.assertIn("reference differs", self.validate()[1][0]["error"])

    def test_missing_and_manifest_mismatch(self):
        self.wav.unlink()
        self.assertEqual(len(self.validate()[1]), 1)
        (self.recordings / "corpus.json").write_text("changed")
        self.assertEqual(len(self.validate()[1]), 2)

    def test_missing_case_prevents_any_engine_run_and_total_score(self):
        self.wav.unlink()
        args = argparse.Namespace(corpus=self.corpus, recordings=self.recordings, split="dev",
                                  label="test", engine="whisper", device="cpu", threads=4,
                                  no_itn=False, engine_bin=Path("/bin/echo"), model=self.corpus, timeout=1)
        with patch.object(ep, "load_corpus", return_value={"cases": [self.case]}), patch.object(ep, "run_engine") as run:
            result = ep.evaluate(args)
        run.assert_not_called()
        self.assertEqual(result["status"], "incomplete")
        self.assertIsNone(result["aggregate"])

    def test_engine_failure_prevents_aggregate(self):
        args = argparse.Namespace(corpus=self.corpus, recordings=self.recordings, split="dev",
                                  label="test", engine="whisper", device="cpu", threads=4,
                                  no_itn=False, engine_bin=Path("/bin/false"), model=self.corpus, timeout=1)
        with patch.object(ep, "load_corpus", return_value={"cases": [self.case]}):
            result = ep.evaluate(args)
        self.assertEqual(result["status"], "incomplete")
        self.assertIsNone(result["aggregate"])
        self.assertEqual(result["scored_utterances"], 0)

    def test_complete_pipeline_sends_only_audio_and_runtime_options(self):
        args = argparse.Namespace(corpus=self.corpus, recordings=self.recordings, split="dev",
                                  label="test", engine="whisper", device="cpu", threads=4,
                                  no_itn=False, engine_bin=Path("/bin/echo"), model=self.corpus, timeout=1)
        with patch.object(ep, "load_corpus", return_value={"cases": [self.case]}), patch.object(
                ep, "run_engine", return_value={"raw_output": self.case["reference"],
                "elapsed_process_seconds": 0.1, "peak_rss_kib": None, "stderr": ""}) as run:
            result = ep.evaluate(args)
        cmd, env, _ = run.call_args.args
        sent = json.dumps([cmd, env], ensure_ascii=False)
        self.assertNotIn(self.case["reference"], sent)
        self.assertNotIn(self.case["context"], sent)
        self.assertNotIn(self.case["notes"], sent)
        self.assertIn(str(self.wav.resolve()), cmd)
        self.assertEqual(result["status"], "complete")
        self.assertEqual(result["aggregate"]["cer_percent"], 0)
        self.assertEqual(result["model_sha256"], ep.digest(self.corpus))


if __name__ == "__main__":
    unittest.main()
