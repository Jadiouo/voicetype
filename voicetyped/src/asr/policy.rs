//! Bounded production language policy, separate from the raw ASR evaluation CLI.
//! A language tag is evidence for retrying the audio, not permission to delete
//! Latin text. At most one retry is made, and unrecovered output is not sent.
use anyhow::{ensure, Context, Result};

use super::{Language, Transcriber, Utterance};
use crate::output::contains_unexpected_script;
use crate::postproc::strip_tags;
use crate::postproc::tags::StrippedOutput;

fn unexpected(output: &StrippedOutput) -> bool {
    output
        .language_tag()
        .is_some_and(|tag| !["zh", "en"].contains(&tag))
        || contains_unexpected_script(&output.text)
}

pub fn transcribe(asr: &dyn Transcriber, samples: &[f32]) -> Result<Option<String>> {
    let first = strip_tags(&asr.transcribe(Utterance {
        samples,
        language: None,
    })?);
    if first.is_no_speech() {
        return Ok(None);
    }
    if !unexpected(&first) {
        return Ok(Some(first.text));
    }
    ensure!(
        asr.supports_language_hint(),
        "辨識結果包含不支援的語言；此引擎不支援語言重試，未送出文字，請用中文或英文再說一次"
    );
    tracing::warn!(
        language = first.language_tag(),
        "unexpected ASR language; retrying once in Chinese"
    );
    let retried = strip_tags(
        &asr.transcribe(Utterance {
            samples,
            language: Some(Language::Chinese),
        })
        .context("語言重辨識失敗，未送出文字；請用中文或英文再說一次")?,
    );
    ensure!(
        !retried.is_no_speech() && !unexpected(&retried),
        "重辨識仍不是可確認的中英文結果，未送出文字；請再說一次"
    );
    Ok(Some(retried.text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Scripted {
        outputs: Vec<&'static str>,
        calls: Mutex<Vec<Option<Language>>>,
    }
    impl Scripted {
        fn new(outputs: Vec<&'static str>) -> Self {
            Self {
                outputs,
                calls: Mutex::new(vec![]),
            }
        }
    }
    impl Transcriber for Scripted {
        fn transcribe(&self, utterance: Utterance<'_>) -> Result<String> {
            assert_eq!(utterance.samples, [0.25, -0.25]);
            let mut calls = self.calls.lock().unwrap();
            let output = self
                .outputs
                .get(calls.len())
                .expect("unexpected extra retry");
            calls.push(utterance.language);
            if *output == "ERROR" {
                anyhow::bail!("fake engine error");
            }
            Ok((*output).into())
        }
        fn name(&self) -> &str {
            "scripted"
        }
    }
    const AUDIO: &[f32] = &[0.25, -0.25];

    #[test]
    fn normal_english_mixed_and_possible_pinyin_are_never_retried_or_deleted() {
        for raw in [
            "<|en|>push to GitHub",
            "<|zh|>maybe 这个东西 push 到 GitHub",
            "<|zh|>wo xiang push dao GitHub",
            "<|NEUTRAL|>hello",
        ] {
            let fake = Scripted::new(vec![raw]);
            assert_eq!(
                transcribe(&fake, AUDIO).unwrap(),
                Some(strip_tags(raw).text)
            );
            assert_eq!(*fake.calls.lock().unwrap(), vec![None]);
        }
    }
    #[test]
    fn unexpected_tag_or_script_redecodes_same_audio_once() {
        for raw in [
            "<|ja|>漢字",
            "<|yue|>廣東話",
            "<|zh|>これは",
            "<|ko|>한글",
            "テスト",
        ] {
            let fake = Scripted::new(vec![raw, "<|zh|>這個 branch 已經 push 到 GitHub"]);
            assert_eq!(
                transcribe(&fake, AUDIO).unwrap().unwrap(),
                "這個 branch 已經 push 到 GitHub"
            );
            assert_eq!(
                *fake.calls.lock().unwrap(),
                vec![None, Some(Language::Chinese)]
            );
        }
    }
    #[test]
    fn failed_retry_returns_error_without_original_or_partial_output() {
        for retry in ["<|ja|>漢字", "<|zh|>テスト GitHub", "<|nospeech|>", "ERROR"] {
            let fake = Scripted::new(vec!["<|ja|>テスト", retry]);
            assert!(transcribe(&fake, AUDIO).is_err());
            assert_eq!(fake.calls.lock().unwrap().len(), 2);
        }
    }
    #[test]
    fn silence_does_not_trigger_a_second_hallucination_attempt() {
        let fake = Scripted::new(vec!["<|ja|><|nospeech|>"]);
        assert_eq!(transcribe(&fake, AUDIO).unwrap(), None);
        assert_eq!(*fake.calls.lock().unwrap(), vec![None]);
    }

    #[test]
    fn unsupported_language_hint_fails_before_attempting_retry() {
        struct AutoOnly(Scripted);
        impl Transcriber for AutoOnly {
            fn transcribe(&self, u: Utterance<'_>) -> Result<String> {
                self.0.transcribe(u)
            }
            fn name(&self) -> &str { "auto-only" }
            fn supports_language_hint(&self) -> bool { false }
        }
        let fake = AutoOnly(Scripted::new(vec!["テスト", "<|zh|>unexpected retry"]));
        assert!(transcribe(&fake, AUDIO).is_err());
        assert_eq!(*fake.0.calls.lock().unwrap(), vec![None]);
    }
}
