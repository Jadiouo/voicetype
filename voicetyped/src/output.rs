//! One delivery boundary for recording, ProcessText and isolated text replay.
//! The first conversion makes vocabulary matching consistent; the last one
//! also covers text introduced by personal rules or the optional refiner.
use anyhow::{ensure, Context, Result};

use crate::assistant::Assistant;
use crate::personalization::ContextSnapshot;
use crate::postproc::{Traditional, Vocab};

pub fn process(
    traditional: Option<&Traditional>,
    vocab: &Vocab,
    assistant: &Assistant,
    text: &str,
    scope: &ContextSnapshot,
    mode: Option<&str>,
) -> Result<String> {
    ensure!(text.chars().count() <= 4096, "text too long");
    ensure!(
        mode.is_none_or(|m| ["off", "faithful", "clean"].contains(&m)),
        "invalid mode"
    );
    let traditional = traditional.context("無法輸出繁體：請安裝 OpenCC s2tw 資料檔")?;
    // Use one dictionary version across both conversion boundaries and the
    // correction stage, even if the user saves the vocabulary mid-request.
    let vocab = vocab.snapshot();
    let text = traditional.convert_preserving(text, vocab.names())?;
    let started = std::time::Instant::now();
    let (corrected, terms) = vocab.apply_with_terms(&text);
    tracing::info!(
        changed = corrected != text,
        elapsed_us = started.elapsed().as_micros() as u64,
        "dictionary correction completed"
    );
    let text = assistant.process_with_terms(&corrected, scope, mode, &terms, vocab.names());
    let text = traditional.convert_preserving(&text, vocab.names())?;
    ensure!(
        !contains_unexpected_script(&text),
        "結果仍含日文假名、韓文或注音，未送出文字；請用中文或英文再說一次"
    );
    Ok(text)
}

/// These scripts are unambiguous. Latin is deliberately unrestricted: English,
/// technical names and romanized Chinese cannot be reliably separated by script.
/// Han-only Japanese/Cantonese cannot be inferred from characters either.
pub fn contains_unexpected_script(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(c as u32,
        // Exclude Common punctuation such as U+30FB middle dot and U+30A0.
        0x3041..=0x3096 | 0x3099..=0x309f | 0x30a1..=0x30fa |
        0x30fc..=0x30ff | 0x31f0..=0x31ff | 0xff66..=0xff9f |
        0x1b000..=0x1b16f | 0x1aff0..=0x1afff |
        0x1100..=0x11ff | 0x3130..=0x318f | 0xa960..=0xa97f |
        0xac00..=0xd7af | 0xd7b0..=0xd7ff |
        0x3100..=0x312f | 0x31a0..=0x31bf)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::personalization::Personalization;

    fn empty_vocab() -> Vocab {
        Vocab::load_or_empty(std::path::Path::new(
            "/nonexistent-voicetype-test/vocab.toml",
        ))
    }

    #[test]
    fn explicit_name_spellings_survive_both_conversion_boundaries_and_simplified_input() {
        let traditional = Traditional::load().expect("OpenCC data required");
        let assistant = Assistant::new(Personalization::memory(), None);
        let path =
            std::env::temp_dir().join(format!("voicetype-names-{}.toml", std::process::id()));
        std::fs::write(&path, "names=['游錫堃','GitHub']\n").unwrap();
        let vocab = Vocab::load(&path).unwrap();
        for text in [
            "游錫堃今天会来，GitHub 也要检查。",
            "游锡堃今天会来，GitHub 也要检查。",
        ] {
            assert_eq!(
                process(
                    Some(&traditional),
                    &vocab,
                    &assistant,
                    text,
                    &ContextSnapshot::default(),
                    None
                )
                .unwrap(),
                "游錫堃今天會來，GitHub 也要檢查。"
            );
        }
        std::fs::write(&path, "names=['游錫堃','遊錫堃']\n").unwrap();
        assert_eq!(
            process(
                Some(&traditional),
                &vocab,
                &assistant,
                "游錫堃和遊錫堃不是同一個名字。",
                &ContextSnapshot::default(),
                None
            )
            .unwrap(),
            "游錫堃和遊錫堃不是同一個名字。"
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn shorter_name_variants_cannot_rewrite_explicit_longer_names() {
        let traditional = Traditional::load().expect("OpenCC data required");
        let assistant = Assistant::new(Personalization::memory(), None);
        let path = std::env::temp_dir().join(format!(
            "voicetype-overlapping-names-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            "names=['台積電','臺積電公司','游錫堃','遊錫堃基金會']\n",
        )
        .unwrap();
        let vocab = Vocab::load(&path).unwrap();
        for (input, expected) in [
            (
                "臺積電公司和台積電今天会来。",
                "臺積電公司和台積電今天會來。",
            ),
            (
                "台积电公司和台积电今天会来。",
                "臺積電公司和台積電今天會來。",
            ),
            (
                "遊錫堃基金會和游錫堃今天会来。",
                "遊錫堃基金會和游錫堃今天會來。",
            ),
        ] {
            assert_eq!(
                process(
                    Some(&traditional),
                    &vocab,
                    &assistant,
                    input,
                    &ContextSnapshot::default(),
                    None
                )
                .unwrap(),
                expected
            );
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn explicit_names_override_learned_aliases_without_disabling_other_corrections() {
        let traditional = Traditional::load().expect("OpenCC data required");
        let mut memory = Personalization::memory();
        let scope = ContextSnapshot::default();
        memory.learn("mabe", "maybe", &scope).unwrap();
        memory.learn("游錫堃", "遊錫堃", &scope).unwrap();
        memory.learn("找臺積電", "錯誤", &scope).unwrap();
        let assistant = Assistant::new(memory, None);
        let path = std::env::temp_dir().join(format!(
            "voicetype-learned-names-{}.toml",
            std::process::id()
        ));
        std::fs::write(&path, "names=['Mabe','游錫堃','臺積電公司']\n").unwrap();
        let vocab = Vocab::load(&path).unwrap();
        for mode in [None, Some("off")] {
            for (input, expected) in [
                (
                    "Mabe said mabe it is correct.",
                    "Mabe said maybe it is correct.",
                ),
                ("游锡堃今天会来。", "游錫堃今天會來。"),
                ("找臺積電公司，再找臺積電。", "找臺積電公司，再錯誤。"),
            ] {
                assert_eq!(
                    process(Some(&traditional), &vocab, &assistant, input, &scope, mode).unwrap(),
                    expected
                );
            }
        }
        std::fs::remove_file(&path).unwrap();
        // Removing the optional names restores the original learned behavior.
        assert_eq!(
            process(
                Some(&traditional),
                &vocab,
                &assistant,
                "The name is Mabe.",
                &scope,
                None
            )
            .unwrap(),
            "The name is maybe."
        );
    }

    #[test]
    fn final_conversion_covers_learned_replacement_and_keeps_vocabulary() {
        let traditional = Traditional::load().expect("OpenCC data required");
        let mut memory = Personalization::memory();
        let scope = ContextSnapshot {
            program: "editor".into(),
            ..Default::default()
        };
        memory.learn("錯詞", "软件", &scope).unwrap();
        let assistant = Assistant::new(memory, None);
        assert_eq!(
            process(
                Some(&traditional),
                &empty_vocab(),
                &assistant,
                "这个錯詞已经通过测试，push 到 GitHub。",
                &scope,
                Some("off")
            )
            .unwrap(),
            "這個軟件已經通過測試，push 到 GitHub。"
        );
    }

    #[test]
    fn missing_conversion_is_visible_and_no_fallback_is_delivered() {
        let assistant = Assistant::new(Personalization::memory(), None);
        assert!(process(
            None,
            &empty_vocab(),
            &assistant,
            "这个结果",
            &Default::default(),
            None
        )
        .is_err());
    }

    #[test]
    fn final_boundary_rejects_unexpected_script_from_learned_rules() {
        let traditional = Traditional::load().expect("OpenCC data required");
        let mut memory = Personalization::memory();
        let scope = ContextSnapshot {
            program: "editor".into(),
            ..Default::default()
        };
        memory.learn("錯詞", "テスト", &scope).unwrap();
        let assistant = Assistant::new(memory, None);
        assert!(process(
            Some(&traditional),
            &empty_vocab(),
            &assistant,
            "這個錯詞",
            &scope,
            Some("off")
        )
        .is_err());
    }

    #[test]
    fn allows_english_identifiers_and_does_not_pretend_to_detect_pinyin() {
        for text in [
            "maybe push 到 GitHub",
            "Nav2 Isaac RA-L α=0.5",
            "wo xiang push dao GitHub",
            "Mary・Jane 甲・乙",
        ] {
            assert!(!contains_unexpected_script(text));
        }
        for text in ["hello テスト", "한글", "ㄓㄨˋ", "ｶﾀｶﾅ"] {
            assert!(contains_unexpected_script(text));
        }
    }
}
