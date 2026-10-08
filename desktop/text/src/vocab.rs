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

use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use tracing::{info, warn};

#[derive(Debug, Deserialize)]
struct Table {
    #[serde(default)]
    entry: Vec<Entry>,
    /// Canonical spellings offered to the optional contextual model.
    #[serde(default)]
    terms: Vec<String>,
    /// Explicit name spellings that survive both OpenCC boundaries.
    #[serde(default)]
    names: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Entry {
    wrong: Vec<String>,
    right: String,
}

pub struct Vocab {
    path: Option<PathBuf>,
    cache: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    stamp: Option<FileStamp>,
    data: Arc<VocabSnapshot>,
}

#[derive(Default)]
pub struct VocabSnapshot {
    /// (小寫的 wrong, right)。依 wrong 長度降序 —— 否則
    /// `rate` 會先於 `learning rate` 命中, 留下半個詞。
    rules: Vec<(String, String)>,
    terms: Vec<String>,
    names: Vec<String>,
}

impl Vocab {
    pub fn load(path: &Path) -> Result<Self> {
        let stamp = file_stamp(path)?;
        let data = Arc::new(read_data(path)?);
        info!(rules = data.rules.len(), "詞彙修正表已載入");
        Ok(Self {
            path: Some(path.to_owned()),
            cache: Mutex::new(Cache {
                stamp: Some(stamp),
                data,
            }),
        })
    }

    /// 找不到檔案時回傳空表而不是錯誤 —— 詞彙表是選配。
    pub fn load_or_empty(path: &Path) -> Self {
        Self::load(path).unwrap_or_else(|_| {
            if path.exists() {
                warn!("詞彙表載入失敗，暫用空表；修復後下句自動重載");
            }
            Self {
                path: Some(path.to_owned()),
                cache: Mutex::new(Cache::default()),
            }
        })
    }

    pub fn snapshot(&self) -> Arc<VocabSnapshot> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(path) = &self.path {
            match file_stamp(path) {
                Ok(stamp) if cache.stamp != Some(stamp) => {
                    match read_data(path) {
                        Ok(data) => {
                            info!(rules = data.rules.len(), "詞彙修正表已自動重載");
                            cache.data = Arc::new(data);
                        }
                        Err(_) => warn!("詞彙表更新無效，沿用上一份有效詞庫"),
                    }
                    cache.stamp = Some(stamp);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    cache.data = Arc::new(VocabSnapshot::default());
                    cache.stamp = None;
                }
                _ => (),
            }
        }
        Arc::clone(&cache.data)
    }

    pub fn len(&self) -> usize {
        self.snapshot().rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
        self.apply_with_terms(text).0
    }

    /// One snapshot for both exact replacements and optional model vocabulary.
    pub fn apply_with_terms(&self, text: &str) -> (String, Vec<String>) {
        self.snapshot().apply_with_terms(text)
    }
}

impl VocabSnapshot {
    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn apply_with_terms(&self, text: &str) -> (String, Vec<String>) {
        let data = self;
        if data.rules.is_empty() || text.is_empty() {
            return (text.to_owned(), data.terms.clone());
        }
        let mut out = String::with_capacity(text.len());
        let names = name_spans(text, &data.names);
        let mut pos = 0;
        let mut code_ticks = 0;

        while pos < text.len() {
            let tail = &text[pos..];
            if tail.starts_with('`') {
                let count = tail.bytes().take_while(|b| *b == b'`').count();
                if code_ticks == 0 {
                    code_ticks = count;
                } else if code_ticks == count {
                    code_ticks = 0;
                }
                out.push_str(&tail[..count]);
                pos += count;
                continue;
            }
            let hit = if code_ticks == 0 {
                data.rules
                    .iter()
                    .find_map(|(wrong, right)| match matched_len(tail, wrong) {
                        0 => None,
                        n if token_boundaries(text, pos, n, wrong)
                            && !names
                                .iter()
                                .any(|&(start, end)| start < pos + n && end > pos)
                            && !crate::code_at(text, pos, pos + n) =>
                        {
                            Some((n, right))
                        }
                        _ => None,
                    })
            } else {
                None
            };

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
        (out, data.terms.clone())
    }
}

#[cfg(unix)]
type FileStamp = (u64, u64, i64, i64);
#[cfg(not(unix))]
type FileStamp = Vec<u8>;

fn file_stamp(path: &Path) -> std::io::Result<FileStamp> {
    #[cfg(unix)]
    {
        let meta = std::fs::metadata(path)?;
        Ok((meta.ino(), meta.len(), meta.mtime(), meta.mtime_nsec()))
    }
    #[cfg(not(unix))]
    {
        // Atomic replacements can retain size/timestamps on Windows. Bounded
        // bytes detect them without relying on Unix inode identity.
        let mut data = Vec::new();
        std::fs::File::open(path)?
            .take(128 * 1024 + 1)
            .read_to_end(&mut data)?;
        Ok(data)
    }
}

fn read_data(path: &Path) -> Result<VocabSnapshot> {
    const MAX_BYTES: u64 = 128 * 1024;
    let mut text = String::new();
    let mut file = std::fs::File::open(path)?;
    let before = file.metadata()?;
    file.by_ref()
        .take(MAX_BYTES + 1)
        .read_to_string(&mut text)?;
    let after = file.metadata()?;
    ensure!(
        (before.len(), before.modified()?) == (after.len(), after.modified()?),
        "vocabulary changed during read"
    );
    ensure!(text.len() as u64 <= MAX_BYTES, "vocabulary too large");
    VocabSnapshot::from_toml(&text)
}

impl VocabSnapshot {
    /// Compile the same bounded rules used by live dictation; no ASR/model work.
    pub fn from_toml(text: &str) -> Result<Self> {
        ensure!(text.len() <= 128 * 1024, "vocabulary too large");
        let table: Table = toml::from_str(text).context("invalid vocabulary syntax")?;
        ensure!(
            table.entry.len() <= 1024 && table.terms.len() <= 256 && table.names.len() <= 256,
            "too many terms"
        );
        ensure!(
            table
                .terms
                .iter()
                .chain(&table.names)
                .all(|term| !term.is_empty()
                    && term.chars().count() <= 64
                    && !term.contains(['\0', '\n', '\r'])),
            "invalid canonical term"
        );
        ensure!(
            table.names.iter().all(|name| name.chars().count() >= 2),
            "names must contain at least two characters; use a complete proper name"
        );
        let mut data = VocabSnapshot {
            terms: table.terms,
            names: table.names,
            ..Default::default()
        };
        data.names.sort();
        data.names.dedup();
        data.terms.extend(data.names.iter().cloned());
        if !data.names.is_empty() {
            let traditional =
                super::Traditional::load().context("OpenCC required for name spellings")?;
            let variants: Vec<_> = data
                .names
                .iter()
                .map(|name| {
                    traditional
                        .convert(name)
                        .map(|variant| (variant, name.clone()))
                })
                .collect::<Result<_>>()?;
            for (variant, name) in &variants {
                // A normalized spelling that also names a different configured
                // entity is ambiguous. Preserve explicit source names, don't guess.
                if variant != name
                    && !data.names.contains(variant)
                    && variants
                        .iter()
                        .filter(|(value, _)| value == variant)
                        .count()
                        == 1
                {
                    data.rules.push((variant.to_lowercase(), name.clone()));
                }
            }
        }
        data.names.sort_by_key(|name| std::cmp::Reverse(name.len()));
        for entry in table.entry {
            ensure!(
                !entry.right.is_empty() && entry.right.chars().count() <= 64,
                "invalid replacement"
            );
            data.terms.push(entry.right.clone());
            for wrong in entry.wrong {
                ensure!(
                    !wrong.is_empty() && wrong.chars().count() <= 64,
                    "invalid source"
                );
                data.rules.push((wrong.to_lowercase(), entry.right.clone()));
            }
        }
        ensure!(data.rules.len() <= 4096, "too many aliases");
        data.rules.sort();
        ensure!(
            data.rules
                .windows(2)
                .all(|p| p[0].0 != p[1].0 || p[0].1 == p[1].1),
            "conflicting aliases"
        );
        data.rules.dedup();
        data.rules
            .sort_by_key(|(wrong, _)| std::cmp::Reverse(wrong.chars().count()));
        data.terms.sort();
        data.terms.dedup();
        Ok(data)
    }
}

/// Leftmost, longest exact names are authoritative across conversion and aliases.
/// Return byte ranges so an alias crossing a name boundary is protected too.
pub fn name_spans(text: &str, names: &[String]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    if names.is_empty() {
        return spans;
    }
    let mut pos = 0;
    while pos < text.len() {
        let hit = names
            .iter()
            .filter(|name| !name.is_empty() && text[pos..].starts_with(name.as_str()))
            .filter(|name| token_boundaries(text, pos, name.len(), name))
            .max_by_key(|name| name.len());
        if let Some(name) = hit {
            spans.push((pos, pos + name.len()));
            pos += name.len();
        } else {
            pos += text[pos..]
                .chars()
                .next()
                .expect("nonempty suffix")
                .len_utf8();
        }
    }
    spans
}

fn token_boundaries(text: &str, start: usize, len: usize, pattern: &str) -> bool {
    let latin = |c: char| c.is_ascii_alphanumeric() || c == '_';
    !(pattern.chars().next().is_some_and(latin)
        && text[..start].chars().next_back().is_some_and(latin))
        && !(pattern.chars().next_back().is_some_and(latin)
            && text[start + len..].chars().next().is_some_and(latin))
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
        Vocab {
            path: None,
            cache: Mutex::new(Cache {
                stamp: None,
                data: Arc::new(VocabSnapshot {
                    rules,
                    terms: Vec::new(),
                    names: Vec::new(),
                }),
            }),
        }
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

    #[test]
    fn english_rules_preserve_longer_words_but_allow_chinese_neighbors() {
        let v = vocab(&[("mabe", "maybe"), ("gthub", "GitHub")]);
        assert_eq!(
            v.apply("這個mabe對，push到gthub"),
            "這個maybe對，push到GitHub"
        );
        assert_eq!(v.apply("Mabel mabe_id mygthub"), "Mabel mabe_id mygthub");
    }

    /// 長的規則要先命中, 否則 `rate` 會把 `learning rate` 咬掉一半。
    #[test]
    fn longer_rules_win() {
        let v = vocab(&[("rate", "速率"), ("learning rate", "learning rate")]);
        assert_eq!(
            v.apply("the learning rate is high"),
            "the learning rate is high"
        );
    }

    #[test]
    fn replaces_every_occurrence() {
        let v = vocab(&[("吉特", "git")]);
        assert_eq!(
            v.apply("先吉特 add 再吉特 commit"),
            "先git add 再git commit"
        );
    }

    #[test]
    fn leaves_unrelated_text_alone() {
        let v = vocab(&[("熱力瑞", "learning rate")]);
        let s = "這句話裡沒有任何需要修正的詞";
        assert_eq!(v.apply(s), s);
    }

    #[test]
    fn aliases_preserve_code_paths_and_longer_identifiers() {
        let v = vocab(&[("gthub", "GitHub"), ("應用城市", "應用程式")]);
        assert_eq!(
            v.apply("gthub `gthub` /tmp/gthub gthub_id 應用城市"),
            "GitHub `gthub` /tmp/gthub gthub_id 應用程式"
        );
        assert_eq!(
            v.apply("``gthub`` ```text\ngthub\n``` gthub"),
            "``gthub`` ```text\ngthub\n``` GitHub"
        );
    }

    #[test]
    fn reloads_dictionary_and_terms_without_restarting_and_retains_last_good() {
        let p =
            std::env::temp_dir().join(format!("voicetype-live-vocab-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let v = Vocab::load_or_empty(&p);
        assert_eq!(v.apply("gthub"), "gthub");
        std::fs::write(
            &p,
            "terms=['Antigravity']\n[[entry]]\nwrong=['gthub']\nright='GitHub'\n",
        )
        .unwrap();
        let (out, terms) = v.apply_with_terms("推到gthub");
        assert_eq!(out, "推到GitHub");
        assert!(terms.contains(&"Antigravity".into()) && terms.contains(&"GitHub".into()));
        std::fs::write(&p, "this is a half-written invalid config").unwrap();
        assert_eq!(v.apply("gthub"), "GitHub");
        std::fs::write(&p, "[[entry]]\nwrong=['應用城市']\nright='應用程式'\n").unwrap();
        assert_eq!(v.apply("gthub 應用城市"), "gthub 應用程式");
        std::fs::remove_file(&p).unwrap();
        assert_eq!(v.apply("應用城市"), "應用城市");
    }

    #[test]
    fn rejects_conflicting_aliases_instead_of_selecting_arbitrarily() {
        let p = std::env::temp_dir().join(format!(
            "voicetype-conflict-vocab-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &p,
            "[[entry]]\nwrong=['term']\nright='One'\n[[entry]]\nwrong=['TERM']\nright='Two'\n",
        )
        .unwrap();
        assert!(Vocab::load(&p).is_err());
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn empty_table_is_a_noop() {
        let v = vocab(&[]);
        assert_eq!(v.apply("原封不動"), "原封不動");
    }

    #[test]
    fn explicit_names_block_internal_and_crossing_aliases() {
        let data = VocabSnapshot {
            names: vec!["臺積電公司".into(), "台積電".into()],
            rules: vec![
                ("找臺積電".into(), "錯誤".into()),
                ("臺積電".into(), "台積電".into()),
            ],
            terms: Vec::new(),
        };
        let text = "找臺積電公司，再找臺積電。";
        assert_eq!(data.apply_with_terms(text).0, "找臺積電公司，再錯誤。");
        assert_eq!(
            name_spans("GitHub GitHubber", &["GitHub".into()]),
            vec![(0, 6)]
        );
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
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let path = repo.join("config").join("vocab.toml");
        let v = Vocab::load(&path).expect("內附的 vocab.toml 應該是合法的");
        assert!(!v.is_empty(), "內附的詞彙表不該是空的");
    }
}
