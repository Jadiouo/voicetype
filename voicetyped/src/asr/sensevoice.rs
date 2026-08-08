//! SenseVoice-Small 引擎 (SDD §4.1)。
//!
//! 透過 `shim/sensevoice_shim.cpp` 的 `extern "C"` 介面呼叫 ——
//! 上游是 C++ API 且沒有回傳文字的函式, 理由見 docs/sdd-deviations.md D8。

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_float, c_int};
use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, Result};

use super::{Language, Transcriber, Utterance};
use crate::vad::{SpeechDetector, HOP};

#[repr(C)]
struct VtSvContext {
    _private: [u8; 0],
}

const VT_SV_ERR_INVALID_ARG: c_int = -1;
const VT_SV_ERR_INFERENCE: c_int = -2;
const VT_SV_ERR_BUFFER_TOO_SMALL: c_int = -3;

extern "C" {
    fn vt_sv_init(
        model_path: *const c_char,
        n_threads: c_int,
        use_itn: c_int,
    ) -> *mut VtSvContext;
    fn vt_sv_free(ctx: *mut VtSvContext);
    fn vt_sv_transcribe(
        ctx: *mut VtSvContext,
        samples: *const c_float,
        n_samples: usize,
        language: *const c_char,
        out: *mut c_char,
        out_cap: usize,
    ) -> c_int;
    fn vt_sv_vad_probs(
        ctx: *mut VtSvContext,
        samples: *const c_float,
        n_samples: usize,
        out_probs: *mut c_float,
        out_cap: usize,
    ) -> c_int;
    fn vt_sv_last_error(ctx: *const VtSvContext) -> *const c_char;
}

/// 轉錄結果的緩衝區上限。
///
/// 60 秒的語音頂多幾百個字, 加上標籤與 UTF-8 的三位元組編碼, 64KB 有
/// 兩個數量級的餘裕。緩衝不足時 shim 回錯誤而不是截斷 —— 截斷的中文
/// 會產生半個字元的無效 UTF-8。
const OUTPUT_CAP: usize = 64 * 1024;

/// 執行緒數 (SDD §5.2 的延遲預算以 4 threads 為準)。
///
/// 不用 `nproc`: SDD §2/C4 要求與 `make -j$(nproc)` 共存, 搶滿所有核心
/// 會拖垮使用者正在跑的編譯。真正的隔離由 systemd 的 `AllowedCPUs` 與
/// `CPUWeight` 提供 (§6.2), 這裡只是不主動去搶。
const DEFAULT_THREADS: c_int = 4;

pub struct SenseVoice {
    /// 引擎的 `state` 在推論過程中被改寫, 兩個執行緒同時進去就是
    /// data race。用 Mutex 序列化 —— 這不只是安全需求, 也符合實際:
    /// CPU 推論併發只會讓兩次都變慢, 而 PTT 本來就是一次一段。
    ctx: Mutex<ContextHandle>,
    name: String,
}

struct ContextHandle(*mut VtSvContext);

// SAFETY: 指標只在持有上面那個 Mutex 時被解參考, 且 shim 內部不持有
// 任何執行緒區域狀態。
unsafe impl Send for ContextHandle {}

impl Drop for ContextHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { vt_sv_free(self.0) };
            self.0 = std::ptr::null_mut();
        }
    }
}

impl SenseVoice {
    pub fn load(model_path: &Path) -> Result<Self> {
        // use_itn = 1: 讓引擎做反向文字正規化 (數字寫成阿拉伯數字等)。
        // 這是聽寫想要的形式 —— 「三十二」在程式脈絡下應該是 32。
        Self::load_with_itn(model_path, true)
    }

    /// `use_itn = false` 只給評測用 (SDD §8.1)。
    ///
    /// 語料的參考文本寫的是中文數字 (「明天下午三點」), ITN 會把它輸出成
    /// 「3点」而被 CER 算成兩個錯誤 —— 那是**表示差異**不是辨識錯誤,
    /// 與繁簡摺疊缺件同一類的類別錯誤 (見 eval/evaluate.py 開頭那段記錄)。
    /// 關掉 ITN 才能與上游 CLI 在同一個基準上比較。
    pub fn load_with_itn(model_path: &Path, use_itn: bool) -> Result<Self> {
        if !model_path.exists() {
            return Err(anyhow!("model not found: {}", model_path.display()));
        }
        let c_path = CString::new(model_path.as_os_str().as_encoded_bytes())
            .map_err(|_| anyhow!("model path contains a NUL byte"))?;

        let ctx = unsafe { vt_sv_init(c_path.as_ptr(), DEFAULT_THREADS, use_itn as c_int) };
        if ctx.is_null() {
            return Err(anyhow!(
                "failed to load SenseVoice model: {}",
                model_path.display()
            ));
        }

        let name = format!(
            "sensevoice:{}",
            model_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown".into())
        );

        Ok(Self {
            ctx: Mutex::new(ContextHandle(ctx)),
            name,
        })
    }
}

impl Transcriber for SenseVoice {
    fn transcribe(&self, utterance: Utterance<'_>) -> Result<String> {
        if utterance.samples.is_empty() {
            return Ok(String::new());
        }

        let lang = match utterance.language {
            Some(Language::Chinese) => "zh",
            Some(Language::English) => "en",
            None => "auto",
        };
        let c_lang = CString::new(lang).expect("language tags contain no NUL");

        let mut buf = vec![0u8; OUTPUT_CAP];
        let guard = self
            .ctx
            .lock()
            .map_err(|_| anyhow!("SenseVoice context poisoned by a previous panic"))?;

        let written = unsafe {
            vt_sv_transcribe(
                guard.0,
                utterance.samples.as_ptr(),
                utterance.samples.len(),
                c_lang.as_ptr(),
                buf.as_mut_ptr() as *mut c_char,
                buf.len(),
            )
        };

        if written < 0 {
            return Err(ffi_error(guard.0, written));
        }
        drop(guard);

        buf.truncate(written as usize);
        // 引擎的 vocab 是 UTF-8, 但不信任外部資料的完整性 —— 壞掉的
        // 位元組序列在這裡變成 U+FFFD, 而不是 panic 掉整個 daemon。
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Silero VAD (SDD §4.4)。權重與 ASR 在同一份 GGUF 裡, 所以由引擎而不是
/// 獨立元件提供 —— 判定邏輯本身在 `crate::vad`, 這裡只負責跑網路。
impl SpeechDetector for SenseVoice {
    fn speech_probs(&self, samples: &[f32]) -> Result<Vec<f32>> {
        if samples.is_empty() {
            return Ok(Vec::new());
        }
        let n_windows = samples.len().div_ceil(HOP);
        let mut probs = vec![0.0f32; n_windows];

        let guard = self
            .ctx
            .lock()
            .map_err(|_| anyhow!("SenseVoice context poisoned by a previous panic"))?;

        let written = unsafe {
            vt_sv_vad_probs(
                guard.0,
                samples.as_ptr(),
                samples.len(),
                probs.as_mut_ptr(),
                probs.len(),
            )
        };
        if written < 0 {
            return Err(ffi_error(guard.0, written));
        }
        drop(guard);

        // shim 的窗數公式與這裡相同, 不一致代表 HOP 與 VT_SV_VAD_HOP
        // 脫節 —— 那會讓 locate_speech 的樣本索引全部錯位, 寧可失敗。
        if written as usize != n_windows {
            return Err(anyhow!(
                "VAD window count mismatch: shim returned {written}, expected {n_windows} \
                 (crate::vad::HOP 與 VT_SV_VAD_HOP 不一致?)"
            ));
        }
        Ok(probs)
    }
}

fn ffi_error(ctx: *const VtSvContext, code: c_int) -> anyhow::Error {
    let detail = unsafe { CStr::from_ptr(vt_sv_last_error(ctx)) }
        .to_string_lossy()
        .into_owned();
    let reason = match code {
        VT_SV_ERR_INVALID_ARG => "invalid argument",
        VT_SV_ERR_INFERENCE => "inference failed",
        VT_SV_ERR_BUFFER_TOO_SMALL => "output buffer too small",
        _ => "unknown error",
    };
    anyhow!(
        "SenseVoice: {reason}{}{detail}",
        if detail.is_empty() { "" } else { ": " }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 讀取 eval/record.py 產生的 WAV: RIFF / 16-bit PCM / mono / 16kHz。
    /// 只支援這一種格式 —— 測試資料是我們自己錄的, 不需要通用解析器。
    fn read_wav_s16_mono(path: &Path) -> Option<Vec<f32>> {
        let bytes = std::fs::read(path).ok()?;
        if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return None;
        }
        // 掃 chunk 找 "data" —— arecord 有時會插入 LIST chunk,
        // 寫死 44 bytes 的偏移會讀到垃圾。
        let mut pos = 12;
        while pos + 8 <= bytes.len() {
            let id = &bytes[pos..pos + 4];
            let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().ok()?) as usize;
            let body = pos + 8;
            if id == b"data" {
                let end = (body + size).min(bytes.len());
                return Some(
                    bytes[body..end]
                        .chunks_exact(2)
                        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                        .collect(),
                );
            }
            pos = body + size + (size & 1);
        }
        None
    }

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    /// FFI 的端到端驗證: 載入模型 → 轉錄真實錄音 → 檢查輸出。
    ///
    /// 這是整個 daemon 唯一的 unsafe 邊界, 而且錯了不會 panic 而是
    /// 產出亂碼, 所以值得用真實資料測。缺模型或錄音時跳過 —— CI 上
    /// 不會有 291MB 的模型檔。
    #[test]
    fn transcribes_recorded_chinese() {
        let root = repo_root();
        let model = std::env::var("VOICETYPE_MODEL")
            .map(PathBuf::from)
            .unwrap_or_else(|_| root.join("models").join("sense-voice-small-q8_0.gguf"));
        let wav = root.join("eval/recordings/zh_pure/001.wav");

        if !model.exists() || !wav.exists() {
            eprintln!("skipping: 缺少模型或錄音 ({})", model.display());
            return;
        }

        let samples = read_wav_s16_mono(&wav).expect("讀取測試錄音");
        assert!(samples.len() > 16_000, "錄音太短, 可能解析錯誤");

        let engine = SenseVoice::load(&model).expect("載入模型");
        let raw = engine
            .transcribe(Utterance {
                samples: &samples,
                language: None,
            })
            .expect("轉錄");

        // shim 刻意保留開頭的標籤 (need_prefix=true), 語言標籤是
        // per-app profile 的判斷依據 (SDD §4.5)。
        assert!(
            raw.contains("<|zh|>"),
            "原始輸出應保留語言標籤, 實得: {raw:?}"
        );

        let stripped = crate::postproc::strip_tags(&raw);
        assert_eq!(stripped.language_tag(), Some("zh"));
        // 參考文本: 明天下午三點我們開會討論這件事情 (引擎輸出簡體)
        assert!(
            stripped.text.contains("明天下午"),
            "轉錄結果不符預期: {:?}",
            stripped.text
        );
    }

    /// 空輸入不該進到引擎。
    #[test]
    fn empty_input_returns_empty() {
        let root = repo_root();
        let model = root.join("models").join("sense-voice-small-q8_0.gguf");
        if !model.exists() {
            return;
        }
        let engine = SenseVoice::load(&model).expect("載入模型");
        let out = engine
            .transcribe(Utterance {
                samples: &[],
                language: None,
            })
            .unwrap();
        assert!(out.is_empty());
    }

    fn load_for_vad() -> Option<(SenseVoice, PathBuf)> {
        let root = repo_root();
        let model = std::env::var("VOICETYPE_MODEL")
            .map(PathBuf::from)
            .unwrap_or_else(|_| root.join("models").join("sense-voice-small-q8_0.gguf"));
        if !model.exists() {
            eprintln!("skipping: 缺少模型 ({})", model.display());
            return None;
        }
        Some((SenseVoice::load(&model).expect("載入模型"), root))
    }

    /// 真實錄音上 VAD 要找得到語音, 而且要**修剪掉**東西 ——
    /// 錄音首尾必然有靜音 (按下熱鍵到開口、說完到放開)。
    #[test]
    fn vad_finds_speech_in_a_real_recording() {
        let Some((engine, root)) = load_for_vad() else {
            return;
        };
        let wav = root.join("eval/recordings/zh_pure/001.wav");
        if !wav.exists() {
            eprintln!("skipping: 缺少錄音");
            return;
        }
        let samples = read_wav_s16_mono(&wav).expect("讀取測試錄音");

        let t = std::time::Instant::now();
        let probs = engine.speech_probs(&samples).expect("VAD 掃描");
        let elapsed = t.elapsed();

        assert_eq!(probs.len(), samples.len().div_ceil(HOP));
        assert!(
            probs.iter().all(|p| (0.0..=1.0).contains(p)),
            "機率應落在 [0,1]"
        );

        let cfg = crate::vad::VadConfig::default();
        let speech = crate::vad::locate_speech(&probs, samples.len(), &cfg)
            .expect("真實語音應被偵測到");
        assert!(
            speech.len() < samples.len(),
            "首尾應有靜音可修剪: {} / {}",
            speech.len(),
            samples.len()
        );

        // VAD 的成本直接加在 SDD §1.3 的 P50 < 400ms 預算上, 值得看得見。
        eprintln!(
            "VAD: {:.2}s 音訊 / {} 窗 / {:?} ({:.1}× realtime), 修剪後 {:.2}s",
            samples.len() as f32 / 16_000.0,
            probs.len(),
            elapsed,
            (samples.len() as f32 / 16_000.0) / elapsed.as_secs_f32(),
            speech.len() as f32 / 16_000.0,
        );
    }

    /// 誤觸熱鍵的情境: 沒有人說話, 只有底噪。
    ///
    /// 這是整個 VAD 存在的理由 —— M0 實測這種輸入會讓引擎吐出「我.」
    /// 並直接 commit 到游標處。測試同時印出未修剪時引擎的輸出, 讓那個
    /// 行為留下記錄而不只是 devlog 裡的一句話。
    #[test]
    fn vad_rejects_silence_that_would_hallucinate() {
        let Some((engine, _)) = load_for_vad() else {
            return;
        };
        // 3 秒的低幅底噪。純零過於理想 —— 真實麥克風永遠有本底,
        // 而 VAD 必須在有本底的情況下仍然判定為靜音。
        let n = 16_000 * 3;
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let noise: Vec<f32> = (0..n)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                // ±0.002 ≈ -54 dBFS, 安靜房間的典型本底。
                ((seed >> 40) as f32 / 8_388_608.0 - 1.0) * 0.002
            })
            .collect();

        let raw = engine
            .transcribe(Utterance {
                samples: &noise,
                language: None,
            })
            .expect("轉錄");
        eprintln!(
            "沒有 VAD 時引擎對靜音的輸出: {:?}",
            crate::postproc::strip_tags(&raw).text
        );

        let probs = engine.speech_probs(&noise).expect("VAD 掃描");
        let got = crate::vad::locate_speech(&probs, noise.len(), &crate::vad::VadConfig::default());
        let peak = probs.iter().cloned().fold(0.0f32, f32::max);
        assert_eq!(
            got, None,
            "靜音不該被判成語音 (峰值機率 {peak:.3})"
        );
    }

    /// LSTM 狀態必須每次掃描歸零, 否則上一段語音的尾巴會污染下一段的
    /// 開頭 —— 而「誤觸熱鍵剛好接在一次正常聽寫之後」正是要防的情境。
    #[test]
    fn vad_state_does_not_leak_between_scans() {
        let Some((engine, root)) = load_for_vad() else {
            return;
        };
        let wav = root.join("eval/recordings/zh_pure/001.wav");
        if !wav.exists() {
            return;
        }
        let speech = read_wav_s16_mono(&wav).expect("讀取測試錄音");
        let silence = vec![0.0f32; 16_000];

        let first = engine.speech_probs(&silence).expect("VAD 掃描");
        engine.speech_probs(&speech).expect("VAD 掃描");
        let after = engine.speech_probs(&silence).expect("VAD 掃描");

        assert_eq!(first, after, "同樣的輸入必須得到同樣的機率");
    }

    /// VAD 不能丟掉真實的語音。
    ///
    /// 這是接上 VAD 的主要風險: 起點判定天生偏晚 (要累積證據才觸發),
    /// 剪過頭就吃掉第一個音節 —— 而那正是 pre-roll (§4.4) 當初花
    /// 500ms 緩衝去防的東西, 在管線末端又剪回去就白做了。最壞的情況是
    /// 整句被判成靜音, 使用者說了話卻什麼都沒出現。
    ///
    /// 斷言只有一條: **沒有任何一句被判成無語音**。修剪造成的逐字差異
    /// 不在這裡斷言 —— 實測 50 句有 16 句不同, 但有好有壞
    /// (`depend`→`dependency` 變好, `batch`→`bach` 變差), 逐字相同是
    /// 錯的判準。淨影響用 CER 量, 那是 eval/evaluate.py 的職責:
    ///
    ///   python3 eval/evaluate.py --engine-type daemon
    ///   python3 eval/evaluate.py --engine-type daemon-novad
    #[test]
    fn vad_never_discards_a_real_utterance() {
        let Some((engine, root)) = load_for_vad() else {
            return;
        };
        let cfg = crate::vad::VadConfig::default();

        let mut checked = 0usize;
        let mut discarded = Vec::new();
        let mut kept_ratio = 0.0f32;

        for set in ["zh_pure", "en_pure", "mixed", "filler"] {
            let dir = root.join("eval/recordings").join(set);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut wavs: Vec<_> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "wav"))
                .collect();
            wavs.sort();

            for wav in wavs {
                let Some(samples) = read_wav_s16_mono(&wav) else {
                    continue;
                };
                let probs = engine.speech_probs(&samples).expect("VAD 掃描");
                checked += 1;
                match crate::vad::locate_speech(&probs, samples.len(), &cfg) {
                    Some(speech) => {
                        kept_ratio += speech.len() as f32 / samples.len() as f32;
                    }
                    None => discarded.push(wav.clone()),
                }
            }
        }

        if checked == 0 {
            eprintln!("skipping: 缺少 eval 錄音");
            return;
        }
        eprintln!(
            "VAD: {checked} 句, {} 句被判成靜音, 平均保留 {:.0}%",
            discarded.len(),
            100.0 * kept_ratio / checked as f32
        );
        assert!(
            discarded.is_empty(),
            "這些錄音有人在說話, 卻被 VAD 判成靜音: {discarded:?}"
        );
    }

    #[test]
    fn missing_model_is_an_error() {
        let err = SenseVoice::load(Path::new("/nonexistent/model.gguf"));
        assert!(err.is_err());
    }
}
