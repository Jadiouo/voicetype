//! 語音活動偵測 (SDD §4.4)。
//!
//! **這是正確性需求, 不是延遲優化。** SDD §4.4 把 VAD 的用途寫成「修剪
//! 靜音降低延遲」與「偵測空錄音」, 但 M0 的實測顯示前者是次要的:
//! 對著靜音按住熱鍵錄 3 秒, 引擎輸出「我.」而不是空字串。非自迴歸的
//! CTC 解碼沒有「什麼都不輸出」這個自然出口, 靜音會被硬解成最像的
//! token。誤觸熱鍵因此會把幻覺文字直接 commit 到游標處。
//!
//! 分段策略與上游 `main.cc` 不同, 刻意的:
//!
//! 上游會把一段錄音切成**多個**語音片段分別辨識, 丟掉片段之間的靜音。
//! PTT 不能這樣做 —— 使用者按住熱鍵時句中的停頓 (思考、換氣) 是說話的
//! 一部分, 切掉會讓「那個... 檔案放哪」變成兩句無關的話送進引擎, 上下文
//! 斷裂反而傷準確度。這裡只做兩件事:
//!
//! 1. 判斷整段**有沒有**足夠的語音 (沒有 → 不進引擎, 回空結果);
//! 2. 修剪首尾靜音, 句中一律保留。

use anyhow::Result;

/// 每個機率窗前進的樣本數, 必須與 shim 的 `VT_SV_VAD_HOP` 一致。
/// 16kHz 下是 32ms。
pub const HOP: usize = 512;

const SAMPLE_RATE: usize = 16_000;

/// VAD 的判定參數。
#[derive(Debug, Clone, Copy)]
pub struct VadConfig {
    /// 進入語音狀態的機率門檻。
    pub threshold: f32,
    /// 離開語音狀態的機率門檻。低於進入門檻形成遲滯 ——
    /// 單一門檻會讓機率在邊界抖動時反覆切換, 把一句話切成碎片。
    pub neg_threshold: f32,
    /// 整段累計語音短於這個長度就視為空錄音。
    pub min_speech_ms: u32,
    /// 修剪後在語音前後各保留的靜音。VAD 的起點判定天生偏晚
    /// (要累積證據才會觸發), 貼著邊界切會吃掉第一個音節的起始輔音。
    pub speech_pad_ms: u32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            // 0.5 / 0.35 是 Silero 上游的建議值, 也是 SenseVoice.cpp
            // main.cc 的預設。沒有理由在還沒實測前偏離。
            threshold: 0.5,
            neg_threshold: 0.35,
            // 250ms 約是一個字的長度。比這更短的「語音」多半是敲鍵盤、
            // 椅子聲這類瞬時噪音 —— 而那正是誤觸熱鍵時會收到的東西。
            min_speech_ms: 250,
            speech_pad_ms: 200,
        }
    }
}

/// 修剪結果。索引以 sample 為單位, 對應傳入的 16kHz 音訊。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Speech {
    pub start: usize,
    pub end: usize,
}

impl Speech {
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
}

/// 產生逐窗語音機率的能力。實作由 ASR 引擎提供 —— Silero 的權重就在
/// SenseVoice 的 GGUF 裡, 共用同一份 context 而不是再載一個模型。
pub trait SpeechDetector: Send + Sync {
    /// `samples` 是 16kHz mono f32。回傳每 [`HOP`] 個樣本一個 [0,1] 機率。
    fn speech_probs(&self, samples: &[f32]) -> Result<Vec<f32>>;
}

/// 從逐窗機率算出語音區間。`None` 代表整段沒有足夠的語音。
///
/// 純函式, 與引擎無關 —— VAD 的判定邏輯全部的錯誤都能在這裡測到,
/// 不需要 291MB 的模型。
pub fn locate_speech(probs: &[f32], n_samples: usize, cfg: &VadConfig) -> Option<Speech> {
    let mut first: Option<usize> = None;
    let mut last: Option<usize> = None;
    let mut active = false;

    for (i, &p) in probs.iter().enumerate() {
        // 遲滯: 未觸發時看 threshold, 已觸發時看較低的 neg_threshold。
        active = if active {
            p >= cfg.neg_threshold
        } else {
            p >= cfg.threshold
        };
        if active {
            first.get_or_insert(i);
            last = Some(i);
        }
    }

    // 「有沒有語音」只數**真正超過 threshold** 的窗, 不數遲滯撐住的。
    //
    // 這裡踩過一次坑: 原本數的是遲滯後的 active 窗數, 結果安靜房間的
    // 底噪被判成語音 —— 實測 4 秒環境音只有 7/125 窗超過 0.5, 但底噪的
    // p90 是 0.407, 落在 neg_threshold(0.35) 與 threshold(0.5) 之間。
    // 一個偶發尖峰觸發之後, 遲滯就把後面幾十個窗全部算成語音。
    //
    // 遲滯的職責是決定**邊界** (別把句中的抖動切開), 不是決定
    // **有沒有**。拿它的輸出去做存在性判斷, 等於把 neg_threshold
    // 悄悄變成實際門檻。
    let strong = probs.iter().filter(|&&p| p >= cfg.threshold).count();
    if strong < ms_to_windows(cfg.min_speech_ms) {
        return None;
    }

    let (first, last) = (first?, last?);
    let pad = ms_to_samples(cfg.speech_pad_ms);
    let start = (first * HOP).saturating_sub(pad);
    // last 是**窗的起點**, 該窗涵蓋到 (last+1)*HOP。
    let end = ((last + 1) * HOP + pad).min(n_samples);

    if end <= start {
        return None;
    }
    Some(Speech { start, end })
}

/// 掃描並修剪。`Ok(None)` 表示這段錄音沒有語音, 不該送進引擎。
pub fn trim(detector: &dyn SpeechDetector, samples: &[f32], cfg: &VadConfig) -> Result<Option<Speech>> {
    if samples.is_empty() {
        return Ok(None);
    }
    let probs = detector.speech_probs(samples)?;
    Ok(locate_speech(&probs, samples.len(), cfg))
}

fn ms_to_samples(ms: u32) -> usize {
    ms as usize * SAMPLE_RATE / 1000
}

fn ms_to_windows(ms: u32) -> usize {
    // 無條件進位: min_speech 是「至少要有這麼多語音」, 向下取整會
    // 讓門檻鬆掉一個窗。
    ms_to_samples(ms).div_ceil(HOP)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> VadConfig {
        VadConfig::default()
    }

    /// 全靜音 → 不進引擎。這是這個模組存在的理由 (見模組說明)。
    #[test]
    fn silence_yields_nothing() {
        let probs = vec![0.01; 100];
        assert_eq!(locate_speech(&probs, 100 * HOP, &cfg()), None);
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert_eq!(locate_speech(&[], 0, &cfg()), None);
    }

    /// 瞬時噪音 (敲鍵盤、椅子聲) 撐不過 min_speech_ms。
    #[test]
    fn a_brief_spike_is_not_speech() {
        let mut probs = vec![0.02; 100];
        // 3 窗 ≈ 96ms, 短於預設的 250ms。
        for p in probs.iter_mut().skip(40).take(3) {
            *p = 0.95;
        }
        assert_eq!(locate_speech(&probs, 100 * HOP, &cfg()), None);
    }

    #[test]
    fn speech_in_the_middle_is_trimmed_on_both_sides() {
        let mut probs = vec![0.02; 100];
        for p in probs.iter_mut().skip(40).take(20) {
            *p = 0.95;
        }
        let n = 100 * HOP;
        let got = locate_speech(&probs, n, &cfg()).expect("應偵測到語音");

        let pad = ms_to_samples(cfg().speech_pad_ms);
        assert_eq!(got.start, 40 * HOP - pad);
        assert_eq!(got.end, 60 * HOP + pad);
        assert!(got.len() < n, "修剪後應短於原始長度");
    }

    /// 句中的停頓必須保留 —— PTT 的使用者還按著熱鍵, 那是同一句話。
    #[test]
    fn a_pause_inside_the_utterance_is_kept() {
        let mut probs = vec![0.02; 120];
        for p in probs.iter_mut().skip(20).take(15) {
            *p = 0.95;
        }
        // 30 窗 ≈ 1 秒的停頓
        for p in probs.iter_mut().skip(65).take(15) {
            *p = 0.95;
        }
        let got = locate_speech(&probs, 120 * HOP, &cfg()).expect("應偵測到語音");
        assert!(
            got.start <= 20 * HOP && got.end >= 80 * HOP,
            "區間應橫跨兩段語音與中間的停頓, 實得 {got:?}"
        );
    }

    /// 語音貼著開頭與結尾時, padding 不能溢出音訊邊界。
    #[test]
    fn padding_is_clamped_to_the_recording() {
        let probs = vec![0.95; 40];
        let n = 40 * HOP;
        let got = locate_speech(&probs, n, &cfg()).expect("應偵測到語音");
        assert_eq!(got.start, 0);
        assert_eq!(got.end, n);
    }

    /// 遲滯: 機率在門檻附近抖動時不該把一句話切成碎片。
    #[test]
    fn hysteresis_bridges_dips_between_the_thresholds() {
        let mut probs = vec![0.02; 100];
        for (i, p) in probs.iter_mut().enumerate().skip(30).take(30) {
            // 0.4 落在 neg_threshold(0.35) 與 threshold(0.5) 之間:
            // 已觸發就維持, 沒觸發則不啟動。
            *p = if i % 3 == 0 { 0.4 } else { 0.9 };
        }
        let got = locate_speech(&probs, 100 * HOP, &cfg()).expect("應偵測到語音");
        assert_eq!(got.end, 60 * HOP + ms_to_samples(cfg().speech_pad_ms));
    }

    /// 單一門檻下會被算成語音的抖動, 在遲滯下不該啟動。
    #[test]
    fn dips_alone_never_trigger() {
        let probs = vec![0.4; 100];
        assert_eq!(locate_speech(&probs, 100 * HOP, &cfg()), None);
    }

    /// 安靜房間的底噪 + 一個偶發尖峰, 不是語音。
    ///
    /// 這是實測抓到的迴歸 (見 locate_speech 裡的說明): 4 秒環境音只有
    /// 7/125 窗超過 0.5, 但底噪的 p90 是 0.407 —— 落在遲滯的兩個門檻
    /// 之間。原本的實作讓那一個尖峰觸發之後, 遲滯把後面所有窗都算成
    /// 語音, 於是靜靜坐著也會被 commit 一整句幻覺文字。
    #[test]
    fn room_tone_with_one_spike_is_not_speech() {
        let mut probs = vec![0.40; 125];
        // 底噪的自然起伏
        for (i, p) in probs.iter_mut().enumerate() {
            *p = if i % 4 == 0 { 0.17 } else { 0.40 };
        }
        // 偶發尖峰: 7 個窗超過門檻, 少於 min_speech 要求的 8 個
        for p in probs.iter_mut().skip(40).take(7) {
            *p = 0.55;
        }
        assert_eq!(
            locate_speech(&probs, 125 * HOP, &cfg()),
            None,
            "底噪撐起來的遲滯不該算成語音"
        );
    }

    /// 但真正說話時, 遲滯仍然要能跨過句中的短暫低谷。
    ///
    /// 與上一個測試的差別只在強證據的數量 —— 這正是修正後的判準。
    #[test]
    fn real_speech_survives_the_stricter_count() {
        let mut probs = vec![0.40; 125];
        for p in probs.iter_mut().skip(30).take(40) {
            *p = 0.92;
        }
        probs[45] = 0.38; // 句中換氣
        let got = locate_speech(&probs, 125 * HOP, &cfg()).expect("應偵測到語音");
        assert!(
            got.start <= 30 * HOP && got.end >= 70 * HOP,
            "換氣不該把語音切斷: {got:?}"
        );
    }

    #[test]
    fn min_speech_rounds_up() {
        // 250ms / 32ms = 7.8 窗 → 需要 8 窗
        assert_eq!(ms_to_windows(250), 8);
        let mut probs = vec![0.02; 50];
        for p in probs.iter_mut().skip(10).take(7) {
            *p = 0.95;
        }
        assert_eq!(locate_speech(&probs, 50 * HOP, &cfg()), None);
        probs[17] = 0.95;
        assert!(locate_speech(&probs, 50 * HOP, &cfg()).is_some());
    }

    struct Canned(Vec<f32>);
    impl SpeechDetector for Canned {
        fn speech_probs(&self, _samples: &[f32]) -> Result<Vec<f32>> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn trim_short_circuits_on_empty_audio() {
        let d = Canned(vec![0.99; 10]);
        assert_eq!(trim(&d, &[], &cfg()).unwrap(), None);
    }

    #[test]
    fn trim_maps_probs_onto_samples() {
        let mut probs = vec![0.02; 60];
        for p in probs.iter_mut().skip(20).take(20) {
            *p = 0.95;
        }
        let samples = vec![0.0f32; 60 * HOP];
        let got = trim(&Canned(probs), &samples, &cfg()).unwrap().unwrap();
        assert!(got.end <= samples.len());
    }
}
