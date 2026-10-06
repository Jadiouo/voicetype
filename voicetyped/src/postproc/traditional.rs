//! 繁化 (SDD §4.6 ②)。
//!
//! SenseVoice 輸出的是簡體中文, 此階段轉為繁體中文的台灣字形。
//!
//! 使用 OpenCC `s2tw` 保留原本用詞。`s2twp` 的地區詞彙替換會把
//! 「通過測試」改成「透過測試」、行政「窗口」改成「視窗」，甚至把
//! 「腦內存留」中的「內存」誤當電腦記憶體。繁化不應改變詞義。
//! 個別技術用語偏好由使用者的明確詞彙表處理，不在這裡自動推定。
//!
//! 方向只做簡→繁。反向 (t2s) 是評測用的摺疊, 屬於 `eval/evaluate.py`,
//! 兩者不該共用同一段程式碼 —— 它們的正確性標準不同: 這裡要的是
//! 「使用者看到的字」, 那裡要的是「兩邊拉到同一基準」。
//!
//! ## 為什麼是 FFI 而不是純 Rust 表
//!
//! 簡繁不是逐字雙射。「发」對應「發」與「髮」, 選哪個要看詞。OpenCC
//! 的 `STPhrases` 有 92 萬筆詞組, 自己抄一份表等於重寫一個較差的 OpenCC。

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::Mutex;

use anyhow::{Context, Result};
use tracing::warn;

#[allow(non_camel_case_types)]
type opencc_t = *mut c_void;

extern "C" {
    fn opencc_open(config: *const c_char) -> opencc_t;
    fn opencc_close(handle: opencc_t) -> i32;
    fn opencc_convert_utf8(handle: opencc_t, input: *const c_char, length: usize) -> *mut c_char;
    fn opencc_convert_utf8_free(s: *mut c_char);
}

/// OpenCC 的配置檔。系統的 `/usr/share/opencc/` 下。
const CONFIG: &str = "s2tw.json";

pub struct Traditional {
    handle: Mutex<Handle>,
}

struct Handle(opencc_t);

// SAFETY: 指標只在持有 Mutex 時被使用。OpenCC 的 converter 本身沒有
// 執行緒區域狀態, 但上游沒有承諾並行安全, 序列化最省事 —— 一次錄音
// 才轉換一次, 爭用不存在。
unsafe impl Send for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { opencc_close(self.0) };
            self.0 = std::ptr::null_mut();
        }
    }
}

impl Traditional {
    /// Explicit dictionary names are literal spellings. Convert only the gaps
    /// around their longest exact occurrences, so 游錫堃 cannot become 遊錫堃.
    pub fn convert_preserving(&self, text: &str, names: &[String]) -> Result<String> {
        if names.is_empty() {
            return self.convert(text);
        }
        let mut out = String::with_capacity(text.len());
        let mut gap = 0;
        for (start, end) in super::vocab::name_spans(text, names) {
            out.push_str(&self.convert(&text[gap..start])?);
            out.push_str(&text[start..end]);
            gap = end;
        }
        out.push_str(&self.convert(&text[gap..])?);
        Ok(out)
    }

    /// 載入失敗回傳 `None`；正式輸出邊界必須將缺件當成可見錯誤。
    pub fn load() -> Option<Self> {
        let config = CString::new(CONFIG).expect("config name has no NUL");
        let handle = unsafe { opencc_open(config.as_ptr()) };
        // 上游用 (opencc_t)-1 表示失敗, 不是 NULL。
        if handle.is_null() || handle as isize == -1 {
            warn!(
                "OpenCC 載入失敗 ({CONFIG}), 無法保證繁體輸出。\
                 請安裝 opencc 資料檔 (Debian/Ubuntu: libopencc-data)"
            );
            return None;
        }
        Some(Self {
            handle: Mutex::new(Handle(handle)),
        })
    }

    /// 簡體 → 繁體台灣字形，保留用詞。失敗不能冒充已完成繁化。
    pub fn convert(&self, text: &str) -> Result<String> {
        if text.is_empty() {
            return Ok(String::new());
        }
        let input = CString::new(text).context("繁體轉換失敗：文字含 NUL")?;
        let guard = self
            .handle
            .lock()
            .map_err(|_| anyhow::anyhow!("繁體轉換失敗：OpenCC lock poisoned"))?;

        let out = unsafe { opencc_convert_utf8(guard.0, input.as_ptr(), text.len()) };
        if out.is_null() {
            anyhow::bail!("繁體轉換失敗：OpenCC conversion failed");
        }
        let converted = unsafe { CStr::from_ptr(out) }
            .to_str()
            .map(str::to_owned)
            .context("繁體轉換失敗：OpenCC returned invalid UTF-8");
        unsafe { opencc_convert_utf8_free(out) };
        converted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opencc() -> Option<Traditional> {
        let t = Traditional::load();
        if t.is_none() {
            eprintln!("skipping: 系統沒有 OpenCC 資料檔");
        }
        t
    }

    #[test]
    fn converts_characters() {
        let Some(c) = opencc() else { return };
        assert_eq!(
            c.convert("这个结果跟我预期的完全不一样").unwrap(),
            "這個結果跟我預期的完全不一樣"
        );
    }

    #[test]
    fn explicit_names_survive_conversion_without_disabling_other_chinese() {
        let c = Traditional::load().expect("OpenCC data required");
        let names = vec!["游錫堃".into(), "干".into(), "干先生".into()];
        assert_eq!(
            c.convert_preserving("游錫堃今天会来，干先生已经到了。", &names)
                .unwrap(),
            "游錫堃今天會來，干先生已經到了。"
        );
        assert!(c.convert_preserving("游錫堃\0简体", &names).is_err());
    }

    /// 繁化不應把詞彙偏好當成辨識修正；需要換詞時使用明確詞彙表。
    #[test]
    fn preserves_vocabulary_without_automatic_regional_rewriting() {
        let c = Traditional::load().expect("OpenCC data is required for regression validation");
        assert_eq!(c.convert("软件界面里的鼠标").unwrap(), "軟件界面裡的鼠標");
    }

    #[test]
    fn preserves_passed_tests_administrative_contacts_and_documents() {
        let c = Traditional::load().expect("OpenCC data is required for regression validation");
        assert_eq!(
            c.convert("这个版本已经通过测试，行政窗口会收文件。")
                .unwrap(),
            "這個版本已經通過測試，行政窗口會收文件。"
        );
    }

    #[test]
    fn avoids_memory_replacement_across_word_boundaries() {
        let c = Traditional::load().expect("OpenCC data is required for regression validation");
        assert_eq!(
            c.convert("这件事仍在脑内存留。").unwrap(),
            "這件事仍在腦內存留。"
        );
    }

    #[test]
    fn retains_taiwanese_character_forms() {
        let c = Traditional::load().expect("OpenCC data is required for regression validation");
        assert_eq!(
            c.convert("这台机器的启动时间").unwrap(),
            "這臺機器的啟動時間"
        );
    }

    /// 「发」的兩個對應要靠詞組決定, 這正是不能用逐字表的理由。
    #[test]
    fn disambiguates_by_phrase() {
        let Some(c) = opencc() else { return };
        assert_eq!(c.convert("头发").unwrap(), "頭髮");
        assert_eq!(c.convert("发现").unwrap(), "發現");
    }

    #[test]
    fn leaves_non_chinese_alone() {
        let Some(c) = opencc() else { return };
        assert_eq!(
            c.convert("git rebase --onto main").unwrap(),
            "git rebase --onto main"
        );
    }

    #[test]
    fn refuses_nul_instead_of_silently_returning_simplified() {
        let c = Traditional::load().expect("OpenCC data required");
        assert!(c.convert("简体\0文字").is_err());
    }

    #[test]
    fn handles_empty_input() {
        let Some(c) = opencc() else { return };
        assert_eq!(c.convert("").unwrap(), "");
    }

    /// 中英夾雜是本專案的主要情境 (SDD §8.1 的 mixed), 英文不能被動到。
    #[test]
    fn preserves_mixed_language_text() {
        let Some(c) = opencc() else { return };
        let got = c.convert("把这个参数的default value改成0.5").unwrap();
        assert!(got.contains("default value"), "英文應原樣保留: {got}");
        assert!(got.contains("這個"), "中文應轉繁: {got}");
    }
}
