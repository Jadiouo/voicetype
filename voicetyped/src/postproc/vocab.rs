//! 詞彙修正表 (SDD §4.6 ③)。
//!
//! ## 這一階能做到什麼
//!
//! 修正表的前提是**錯誤穩定且可枚舉**。R1 的實測顯示中英夾雜的錯誤
//! 大多不滿足這個前提 —— 同一個詞每次錯得不一樣 (`commit`→「抗 make」、
//! `rebase`→`re`、`fallback`→`feedback back`)。那類開放集錯誤要靠模型
//! 或推論階段的詞彙注入, 而 SenseVoice.cpp 不支援 hotwords (D7)。
//!
//! 所以這一階的目標刻意收窄成兩件事:
//!
//! 1. 音譯成**不合法中文詞**的技術詞 (「熱力瑞」→ learning rate);
//! 2. 使用者自己的專案詞彙 —— 這才是修正表真正的價值, 因為只有使用者
//!    知道自己每天會說到哪些專有名詞。
//!
//! ## 為什麼不做得更聰明
//!
//! 誘惑是加上模糊比對或編輯距離, 把 `catch`→`cache` 這種也修掉。不做,
//! 因為修正表沒有語境: 它分不出「try catch」與「用 cache 存起來」。
//! **寧可漏掉, 不可誤改** —— 漏掉的使用者補一個字就好, 誤改的要先被
//! 發現, 而聽寫出來的東西通常不會逐字重讀。

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::{info, warn};

#[derive(Debug, Deserialize)]
struct Table {
    #[serde(default)]
    entry: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Entry {
    wrong: Vec<String>,
    right: String,
}

pub struct Vocab {
    /// (小寫的 wrong, right)。依 wrong 長度降序 —— 否則
    /// `rate` 會先於 `learning rate` 命中, 留下半個詞。
    rules: Vec<(String, String)>,
}

impl Vocab {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("讀取詞彙表 {}", path.display()))?;
        let table: Table = toml::from_str(&text)
            .with_context(|| format!("解析詞彙表 {}", path.display()))?;

        let mut rules = Vec::new();
        for e in table.entry {
            for w in e.wrong {
                if w.is_empty() {
                    warn!("詞彙表有空的 wrong 項目, 略過 (會匹配到每個位置)");
                    continue;
                }
                rules.push((w.to_lowercase(), e.right.clone()));
            }
        }
        rules.sort_by_key(|(wrong, _)| std::cmp::Reverse(wrong.chars().count()));
        info!(rules = rules.len(), "詞彙修正表已載入");
        Ok(Self { rules })
    }

    /// 找不到檔案時回傳空表而不是錯誤 —— 詞彙表是選配。
    pub fn load_or_empty(path: &Path) -> Self {
        if !path.exists() {
            return Self { rules: Vec::new() };
        }
        Self::load(path).unwrap_or_else(|e| {
            // 語法錯誤不該讓整個聽寫停擺, 但也不能靜默 —— 使用者剛改完
            // 檔案卻發現修正沒生效, 會以為是修正表沒用。
            warn!("詞彙表載入失敗, 這次不做詞彙修正: {e:#}");
            Self { rules: Vec::new() }
        })
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// 套用修正。大小寫不敏感 (SDD §4.6 ③)。
    ///
    /// **單次由左至右掃描, 每個位置取最長匹配。** 不是「對每條規則各跑
    /// 一次 replace」—— 那樣的話規則之間會互相破壞: `learning rate` 修
    /// 完之後, `rate` 這條規則還會再命中一次, 把剛修好的詞咬掉一半。
    /// 排序保證同一個起點先試到的就是最長的那條。
    ///
    /// 副作用是替換出來的文字**不會**被重新掃描, 這是想要的: 修正表
    /// 的輸出是最終答案, 不該再被另一條規則改寫。
    pub fn apply(&self, text: &str) -> String {
        if self.rules.is_empty() || text.is_empty() {
            return text.to_owned();
        }
        let mut out = String::with_capacity(text.len());
        let mut pos = 0;

        while pos < text.len() {
            let tail = &text[pos..];
            let hit = self
                .rules
                .iter()
                .find_map(|(wrong, right)| match matched_len(tail, wrong) {
                    0 => None,
                    n => Some((n, right)),
                });

            match hit {
                Some((len, right)) => {
                    out.push_str(right);
                    pos += len;
                }
                None => {
                    let c = tail.chars().next().expect("tail 非空");
                    out.push(c);
                    pos += c.len_utf8();
                }
            }
        }
        out
    }
}

/// 回傳 haystack 開頭與 needle 相符的**位元組長度**, 不符則 0。
fn matched_len(haystack: &str, needle_lower: &str) -> usize {
    let mut h = haystack.char_indices();
    let mut n = needle_lower.chars();
    let mut consumed = 0usize;

    loop {
        let Some(nc) = n.next() else {
            return consumed;
        };
        let Some((idx, hc)) = h.next() else {
            return 0;
        };
        // 逐字元折疊: 對 ASCII 與中文都是一對一, 而多字元展開的
        // 特例 (İ) 在這裡只會比不中, 不會錯位。
        if hc.to_lowercase().next() != Some(nc) {
            return 0;
        }
        consumed = idx + hc.len_utf8();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn vocab(pairs: &[(&str, &str)]) -> Vocab {
        let mut rules: Vec<(String, String)> = pairs
            .iter()
            .map(|(w, r)| (w.to_lowercase(), r.to_string()))
            .collect();
        rules.sort_by_key(|(wrong, _)| std::cmp::Reverse(wrong.chars().count()));
        Vocab { rules }
    }

    #[test]
    fn replaces_a_transliteration() {
        let v = vocab(&[("熱力瑞", "learning rate")]);
        assert_eq!(
            v.apply("我把熱力瑞調低之後就收斂了"),
            "我把learning rate調低之後就收斂了"
        );
    }

    #[test]
    fn is_case_insensitive() {
        let v = vocab(&[("sense voice", "SenseVoice")]);
        assert_eq!(v.apply("用 Sense Voice 跑"), "用 SenseVoice 跑");
        assert_eq!(v.apply("用 SENSE VOICE 跑"), "用 SenseVoice 跑");
    }

    /// 長的規則要先命中, 否則 `rate` 會把 `learning rate` 咬掉一半。
    #[test]
    fn longer_rules_win() {
        let v = vocab(&[("rate", "速率"), ("learning rate", "learning rate")]);
        assert_eq!(v.apply("the learning rate is high"), "the learning rate is high");
    }

    #[test]
    fn replaces_every_occurrence() {
        let v = vocab(&[("吉特", "git")]);
        assert_eq!(v.apply("先吉特 add 再吉特 commit"), "先git add 再git commit");
    }

    #[test]
    fn leaves_unrelated_text_alone() {
        let v = vocab(&[("熱力瑞", "learning rate")]);
        let s = "這句話裡沒有任何需要修正的詞";
        assert_eq!(v.apply(s), s);
    }

    #[test]
    fn empty_table_is_a_noop() {
        let v = vocab(&[]);
        assert_eq!(v.apply("原封不動"), "原封不動");
    }

    /// 替換結果不會被後續規則再次改寫成別的東西。
    #[test]
    fn replacement_is_not_rescanned_by_the_same_rule() {
        let v = vocab(&[("git", "git")]);
        assert_eq!(v.apply("git git git"), "git git git");
    }

    #[test]
    fn handles_multibyte_boundaries() {
        let v = vocab(&[("方選", "function")]);
        assert_eq!(v.apply("這個方選的回傳值"), "這個function的回傳值");
    }

    #[test]
    fn missing_file_yields_empty_table() {
        let v = Vocab::load_or_empty(Path::new("/nonexistent/vocab.toml"));
        assert!(v.is_empty());
    }

    #[test]
    fn broken_toml_yields_empty_table_not_a_crash() {
        let p = std::env::temp_dir().join("voicetype-bad-vocab.toml");
        std::fs::write(&p, "this is not = valid toml [[[").unwrap();
        let v = Vocab::load_or_empty(&p);
        assert!(v.is_empty());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn parses_the_shipped_format() {
        let p = std::env::temp_dir().join("voicetype-test-vocab.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(
            f,
            r#"
[[entry]]
wrong = ["熱力瑞", "热力瑞"]
right = "learning rate"

[[entry]]
wrong = ["吉特"]
right = "git"
"#
        )
        .unwrap();
        let v = Vocab::load(&p).unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v.apply("熱力瑞和吉特"), "learning rate和git");
        let _ = std::fs::remove_file(&p);
    }

    /// 專案自己的詞彙表必須是合法的 —— 它會被安裝到使用者家目錄。
    #[test]
    fn shipped_vocab_file_is_valid() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let path = repo.join("config").join("vocab.toml");
        if !path.exists() {
            return;
        }
        let v = Vocab::load(&path).expect("內附的 vocab.toml 應該是合法的");
        assert!(!v.is_empty(), "內附的詞彙表不該是空的");
    }
}
