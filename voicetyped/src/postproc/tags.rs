//! ① 標籤剝除 (SDD §4.6)。
//!
//! SenseVoice 的輸出帶有結構化標籤, 形如:
//!
//! ```text
//! <|zh|><|NEUTRAL|><|Speech|><|withitn|>幫我看一下 git status
//! ```
//!
//! 依序是語言、情緒、音訊事件、ITN 標記。不剝掉會直接出現在使用者的
//! 文字裡 —— SDD 稱這是「最容易漏掉的一步」。
//!
//! 手寫掃描而非正規式: 樣式是固定的 `<|…|>`, 手寫沒有比較難, 卻能省掉
//! `regex` 這個編譯期成本不小的依賴。

/// 剝除結果。除了乾淨文字, 也保留標籤本身 —— 語言標籤是 §4.5 per-app
/// profile 的判斷依據, 而 `<|nospeech|>` 要轉成 `empty_result` 錯誤。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StrippedOutput {
    pub text: String,
    /// 出現過的標籤 (不含 `<|` `|>`), 依出現順序。
    pub tags: Vec<String>,
}

impl StrippedOutput {
    /// SenseVoice 判定這段音訊沒有語音。
    ///
    /// 使用者誤觸熱鍵時會走到這裡, 應該回 `empty_result` 而不是交付
    /// 一段空字串或雜訊轉錄。
    pub fn is_no_speech(&self) -> bool {
        self.tags
            .iter()
            .any(|t| t.eq_ignore_ascii_case("nospeech") || t.eq_ignore_ascii_case("no_speech"))
            || self.text.trim().is_empty()
    }

    /// 語言標籤 (`zh` / `en` / …), 若有。
    pub fn language_tag(&self) -> Option<&str> {
        // 不把缺少語言標籤時的 NEUTRAL/Speech/withitn 當成語言。
        self.tags.iter().find(|s| (2..=3).contains(&s.len())
            && s.bytes().all(|b| b.is_ascii_lowercase())).map(String::as_str)
    }
}

/// 剝除所有 `<|…|>` 標籤。
pub fn strip_tags(raw: &str) -> StrippedOutput {
    let bytes = raw.as_bytes();
    let mut out = StrippedOutput::default();
    out.text.reserve(raw.len());

    // 以位元組掃描但只以切片複製 —— 逐 `char` 累積會把中文的多位元組
    // 序列拆散。UTF-8 的續位元組都 >= 0x80, 不可能等於 `<` 或 `|`,
    // 所以命中標籤時 `i` 必定落在字元邊界上, 切片是安全的。
    let mut i = 0;
    let mut copy_from = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' && i + 1 < bytes.len() && bytes[i + 1] == b'|' {
            if let Some(end) = find_tag_end(bytes, i + 2) {
                out.text.push_str(&raw[copy_from..i]);
                out.tags.push(raw[i + 2..end].to_string());
                i = end + 2; // 跳過 `|>`
                copy_from = i;
                continue;
            }
            // 沒有對應的結尾: 當成一般文字, 不要吃掉使用者的內容。
        }
        i += 1;
    }
    out.text.push_str(&raw[copy_from..]);

    out.text = out.text.trim().to_string();
    out
}

/// 從 `from` 開始找 `|>`, 回傳 `|` 的位置。標籤內不允許再出現 `<`。
fn find_tag_end(bytes: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < bytes.len() {
        if bytes[i] == b'<' {
            return None; // 標籤未閉合就遇到下一個標籤起始
        }
        if bytes[i] == b'|' && bytes[i + 1] == b'>' {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_sensevoice_prefix() {
        let s = strip_tags("<|zh|><|NEUTRAL|><|Speech|><|withitn|>幫我看一下 git status");
        assert_eq!(s.text, "幫我看一下 git status");
        assert_eq!(s.tags, vec!["zh", "NEUTRAL", "Speech", "withitn"]);
    }

    /// 中文必須完整保留 —— 逐位元組處理會在這裡爆掉。
    #[test]
    fn preserves_multibyte_text() {
        let s = strip_tags("<|zh|>今天天氣很好，我們去散步吧。");
        assert_eq!(s.text, "今天天氣很好，我們去散步吧。");
    }

    #[test]
    fn strips_tags_in_middle_and_end() {
        let s = strip_tags("<|en|>hello <|Laughter|>world<|/Laughter|>");
        assert_eq!(s.text, "hello world");
        assert_eq!(s.tags, vec!["en", "Laughter", "/Laughter"]);
    }

    #[test]
    fn plain_text_untouched() {
        let s = strip_tags("just plain text");
        assert_eq!(s.text, "just plain text");
        assert!(s.tags.is_empty());
    }

    /// 使用者真的講出 "<|" 時不該吃掉他的內容。
    #[test]
    fn unterminated_tag_is_kept_as_text() {
        let s = strip_tags("a <| b");
        assert_eq!(s.text, "a <| b");
        assert!(s.tags.is_empty());
    }

    #[test]
    fn nested_open_does_not_swallow() {
        let s = strip_tags("<|a<|zh|>text");
        assert_eq!(s.text, "<|atext");
        assert_eq!(s.tags, vec!["zh"]);
    }

    #[test]
    fn detects_no_speech() {
        assert!(strip_tags("<|zh|><|nospeech|>").is_no_speech());
        assert!(strip_tags("<|zh|>   ").is_no_speech());
        assert!(!strip_tags("<|zh|>有內容").is_no_speech());
    }

    #[test]
    fn reports_language_tag() {
        assert_eq!(strip_tags("<|zh|><|NEUTRAL|>你好").language_tag(), Some("zh"));
        assert_eq!(strip_tags("<|en|>hi").language_tag(), Some("en"));
        assert_eq!(strip_tags("no tags").language_tag(), None);
        assert_eq!(strip_tags("<|NEUTRAL|><|Speech|><|withitn|>hi").language_tag(), None);
        assert_eq!(strip_tags("<|NEUTRAL|><|yue|>文字").language_tag(), Some("yue"));
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(strip_tags("<|zh|>  你好  ").text, "你好");
    }

    #[test]
    fn empty_input() {
        let s = strip_tags("");
        assert_eq!(s.text, "");
        assert!(s.is_no_speech());
    }
}
