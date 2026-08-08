//! 開發用的空引擎。
//!
//! 讓 IPC → 錄音 → 交付的整條管線可以在真正的 ASR 引擎接上之前先驗證,
//! 也讓 addon 的整合測試不必依賴模型檔。

use anyhow::Result;

use super::{Transcriber, Utterance};

pub struct NullTranscriber;

impl Transcriber for NullTranscriber {
    fn transcribe(&self, utterance: Utterance<'_>) -> Result<String> {
        let secs = utterance.samples.len() as f32 / super::super::audio::TARGET_SAMPLE_RATE as f32;
        Ok(format!("[null asr: {secs:.2}s]"))
    }

    fn name(&self) -> &str {
        "null"
    }
}
