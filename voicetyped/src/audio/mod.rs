//! 音訊擷取管線 (SDD §4.4)。
//!
//! ```text
//! 麥克風 ──► PipeWire ──► cpal ──► Ring Buffer ──► 重採樣 ──► Silero VAD ──► ASR
//!                                   (61s 容量)     (→16kHz)   (crate::vad)
//! ```
//!
//! VAD 在重採樣**之後**: Silero 的窗長 (512 samples) 是以 16kHz 定義的,
//! 在原生取樣率上跑等於改變窗的時間長度。代價是重採樣做了全長而不是
//! 只做語音段, 但重採樣只花 4ms, 不值得為此把順序倒過來。

pub mod capture;
pub mod resample;
pub mod ring;
pub mod wav;

pub use capture::{AudioCapture, StreamMode};

/// SenseVoice 的輸入格式 (SDD §4.4)。
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// 環形緩衝區長度。
///
/// **與 SDD 的偏離**: SDD §4.4/§6.1 寫 30 秒, 但 §4.4 同時把單次錄音
/// 上限訂為 60 秒 —— 兩者不相容, 30 秒的緩衝區會把一段 45 秒的錄音
/// 前半覆蓋掉, 使用者拿到的是後半段的殘句。緩衝區必須至少能容納
/// 「上限 + pre-roll」。
///
/// 代價是記憶體: 48kHz 原生取樣率下約 11.7MB, 而非 SDD §6.1 估的
/// 1.9MB (那是 16kHz 的數字)。總量仍在 400MB 預算內。若要壓回去,
/// 得在音訊回呼中就重採樣到 16kHz —— 那需要在即時執行緒維護重採樣器
/// 狀態, M0 不值得。
pub const RING_SECONDS: f32 = MAX_RECORDING_SECONDS + 1.0;

/// pre-roll 長度 (SDD §4.4)。
pub const PREROLL_MS: u32 = 500;

/// 單次錄音上限 (SDD §4.4)。超過回 `too_long`。
pub const MAX_RECORDING_SECONDS: f32 = 60.0;
