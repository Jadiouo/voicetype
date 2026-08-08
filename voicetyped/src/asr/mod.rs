//! ASR 引擎抽象 (SDD §4.1)。
//!
//! 選型是 SenseVoice-Small int8 / CPU。這裡刻意留一層 trait, 原因寫在
//! SDD §4.1 的「已知弱項」與 §10/R1: 中英夾雜是 SenseVoice 的相對弱項,
//! 若 M0 的實測顯示 `mixed` 語料 CER > 20%, 備案是 per-app 切換到
//! whisper-small。那個備案需要引擎抽象, 而抽象要早於決策點存在, 不然
//! 等於重寫。
//!
//! trait 的成本只有一次動態分派 (每次錄音一次), 相對 250ms 的推論可忽略。

use anyhow::Result;

pub mod null;
#[cfg(feature = "sensevoice")]
pub mod sensevoice;

#[cfg_attr(feature = "sensevoice", allow(unused_imports))]
pub use null::NullTranscriber;
#[cfg(feature = "sensevoice")]
pub use sensevoice::SenseVoice;

/// 一次轉錄的輸入。
pub struct Utterance<'a> {
    /// 16kHz mono f32 (SDD §4.4)。
    pub samples: &'a [f32],
    /// 語言偏好, 來自 per-app profile (SDD §4.5)。`None` 表示自動判定。
    pub language: Option<Language>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Chinese,
    English,
}

pub trait Transcriber: Send + Sync {
    /// 回傳引擎的**原始**輸出 (含 SenseVoice 的結構化標籤)。
    /// 標籤剝除與其餘正規化屬於後處理 (SDD §4.6), 不在引擎層做 ——
    /// 評測工具需要看得到原始輸出。
    fn transcribe(&self, utterance: Utterance<'_>) -> Result<String>;

    /// 用於 log 與評測報告。
    fn name(&self) -> &str;
}
