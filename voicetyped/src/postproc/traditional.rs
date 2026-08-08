//! 繁化 (SDD §4.6 ②)。
//!
//! SenseVoice 輸出的是簡體中文, 而 §1.1 要的是**繁體中文台灣用語**。
//! 這不是偏好問題 —— 一個把「軟體」打成「软件」的聽寫工具在這裡等於
//! 不能用。
//!
//! 用 OpenCC 的 `s2twp` 配置而不是 `s2t`: 前者除了字形還做詞彙轉換
//! (软件→軟體、程序→程式、鼠标→滑鼠), 後者只轉字形, 會留下
//! 「軟件」這種看得懂但不對的詞。
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
const CONFIG: &str = "s2twp.json";

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
    /// 載入失敗回傳 `None` 而不是錯誤。
    ///
    /// 缺 OpenCC 資料檔時 daemon 仍然可用, 只是輸出簡體 —— 那比整個
    /// 聽寫功能不能用好。這與評測工具缺 OpenCC 就中止的處置**刻意
    /// 相反**: 評測給出的是要拿來做決策的數字, 錯的數字比沒有數字糟;
    /// 這裡給出的是使用者當下要的文字, 簡體字比沒有字有用。
    pub fn load() -> Option<Self> {
        let config = CString::new(CONFIG).expect("config name has no NUL");
        let handle = unsafe { opencc_open(config.as_ptr()) };
        // 上游用 (opencc_t)-1 表示失敗, 不是 NULL。
        if handle.is_null() || handle as isize == -1 {
            warn!(
                "OpenCC 載入失敗 ({CONFIG}), 輸出會是簡體中文。\
                 請安裝 opencc 資料檔 (Debian/Ubuntu: libopencc-data)"
            );
            return None;
        }
        Some(Self {
            handle: Mutex::new(Handle(handle)),
        })
    }

    /// 簡體 → 繁體台灣用語。任何失敗都原樣回傳輸入。
    pub fn convert(&self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let Ok(input) = CString::new(text) else {
            // 文字裡有 NUL —— 引擎不該產生, 但不值得為此丟掉整段轉錄。
            return text.to_owned();
        };
        let Ok(guard) = self.handle.lock() else {
            warn!("OpenCC handle poisoned by a previous panic; 略過繁化");
            return text.to_owned();
        };

        let out = unsafe { opencc_convert_utf8(guard.0, input.as_ptr(), text.len()) };
        if out.is_null() {
            warn!("OpenCC 轉換失敗; 輸出原文");
            return text.to_owned();
        }
        let converted = unsafe { CStr::from_ptr(out) }.to_string_lossy().into_owned();
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
        assert_eq!(c.convert("这个结果跟我预期的完全不一样"), "這個結果跟我預期的完全不一樣");
    }

    /// s2twp 的重點: 詞彙而不只是字形。`s2t` 會給出「軟件」。
    #[test]
    fn converts_taiwanese_vocabulary() {
        let Some(c) = opencc() else { return };
        let got = c.convert("这个软件的默认设置");
        assert!(
            got.contains("軟體") && got.contains("預設"),
            "應轉成台灣用語, 實得: {got}"
        );
    }

    /// 「发」的兩個對應要靠詞組決定, 這正是不能用逐字表的理由。
    #[test]
    fn disambiguates_by_phrase() {
        let Some(c) = opencc() else { return };
        assert_eq!(c.convert("头发"), "頭髮");
        assert_eq!(c.convert("发现"), "發現");
    }

    #[test]
    fn leaves_non_chinese_alone() {
        let Some(c) = opencc() else { return };
        assert_eq!(c.convert("git rebase --onto main"), "git rebase --onto main");
    }

    #[test]
    fn handles_empty_input() {
        let Some(c) = opencc() else { return };
        assert_eq!(c.convert(""), "");
    }

    /// 中英夾雜是本專案的主要情境 (SDD §8.1 的 mixed), 英文不能被動到。
    #[test]
    fn preserves_mixed_language_text() {
        let Some(c) = opencc() else { return };
        let got = c.convert("把这个参数的default value改成0.5");
        assert!(got.contains("default value"), "英文應原樣保留: {got}");
        assert!(got.contains("這個"), "中文應轉繁: {got}");
    }
}
