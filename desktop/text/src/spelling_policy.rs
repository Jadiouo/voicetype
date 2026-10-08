//! Shared bounded character-edit policy. Transports never get to rewrite text.
use anyhow::{ensure, Result};
use serde::Deserialize;
pub(crate) const MAX_TEXT: usize = 1024;
pub(crate) const MAX_REPLY: usize = 32 * 1024;
const PROTECTED: &str = "不沒没未無无非勿莫別别零〇一二兩两三四五六七八九十百千萬万億亿兆幾几我你妳您他她它牠祂咱俺買买賣卖";
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    start: usize,
    source: char,
    target: char,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    v: u32,
    id: u64,
    status: String,
    model_calls: u32,
    edits: Vec<Edit>,
}

pub fn accepts(text: &str) -> bool {
    text.chars().count() <= MAX_TEXT && text.chars().any(han)
}

/// Validate the entire response and every original character before applying.
pub fn apply_reply(text: &str, terms: &[&str], id: u64, frame: &[u8]) -> Result<String> {
    ensure!(frame.len() <= MAX_REPLY, "spelling response too large");
    let reply: Reply = serde_json::from_slice(frame)?;
    ensure!(
        reply.v == 1 && reply.id == id && reply.model_calls <= 16,
        "invalid spelling response"
    );
    ensure!(
        [
            "applied",
            "unchanged",
            "skipped",
            "alignment_skip",
            "deadline"
        ]
        .contains(&reply.status.as_str()),
        "invalid spelling status"
    );
    ensure!(
        (reply.status == "applied") == !reply.edits.is_empty(),
        "unexpected spelling edits"
    );
    ensure!(
        reply.edits.is_empty() || reply.model_calls > 0,
        "edits without inference"
    );
    apply(text, &reply.edits, terms)
}
fn han(ch: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&ch)
}

fn apply(text: &str, edits: &[Edit], terms: &[&str]) -> Result<String> {
    ensure!(edits.len() <= 64, "too many spelling edits");
    let positions: Vec<(usize, char)> = text.char_indices().collect();
    let mut result: Vec<char> = positions.iter().map(|(_, ch)| *ch).collect();
    let mut blocked = vec![false; result.len()];
    let mut ticks = 0;
    let mut index = 0;
    let mut quote = None;
    while index < result.len() {
        let ch = result[index];
        if ch == '`' {
            let count = result[index..].iter().take_while(|c| **c == '`').count();
            blocked[index..index + count].fill(true);
            ticks = if ticks == count {
                0
            } else if ticks == 0 {
                count
            } else {
                ticks
            };
            index += count;
            continue;
        }
        if let Some(end) = quote {
            blocked[index] = true;
            let internal_apostrophe = matches!(ch, '\'' | '’')
                && index > 0
                && result[index - 1].is_ascii_alphanumeric()
                && result
                    .get(index + 1)
                    .is_some_and(char::is_ascii_alphanumeric);
            if ch == end && !internal_apostrophe {
                quote = None;
            }
        } else if ticks == 0 {
            let after_word = index > 0 && result[index - 1].is_ascii_alphanumeric();
            quote = match ch {
                '「' => Some('」'),
                '『' => Some('』'),
                '“' => Some('”'),
                '"' => Some('"'),
                '‘' if !after_word => Some('’'),
                '\'' if !after_word => Some('\''),
                _ => None,
            };
            blocked[index] = quote.is_some();
        }
        blocked[index] |= ticks != 0 || PROTECTED.contains(ch);
        index += 1;
    }
    for term in terms {
        for (start, _) in text.match_indices(term) {
            for (i, (pos, _)) in positions.iter().enumerate() {
                if *pos >= start && *pos < start + term.len() {
                    blocked[i] = true;
                }
            }
        }
    }
    let mut byte_start = 0;
    for token in
        text.split_inclusive(|ch: char| ch.is_whitespace() || "，。；、！？「」『』“”".contains(ch))
    {
        if token.contains(['/', '\\', '@', '_', '=', '{', '}'])
            || token.split('.').skip(1).any(|part| {
                part.chars()
                    .next()
                    .is_some_and(|ch| ch.is_ascii_alphanumeric())
            })
        {
            block_span(
                &mut blocked,
                &positions,
                byte_start,
                byte_start + token.len(),
            );
        }
        byte_start += token.len();
    }
    byte_start = 0;
    for clause in text.split_inclusive(['。', '！', '？', '；', '，', ',', '\n']) {
        if [
            "字面",
            "拼法",
            "拼寫",
            "變數名稱",
            "變數名",
            "不要改",
            "保留原樣",
            "刻意取",
        ]
        .iter()
        .any(|cue| clause.contains(cue))
        {
            block_span(
                &mut blocked,
                &positions,
                byte_start,
                byte_start + clause.len(),
            );
        }
        byte_start += clause.len();
    }
    for title in [
        "教授", "老師", "先生", "女士", "小姐", "醫師", "博士", "主任", "經理", "同學",
    ] {
        for (start, _) in text.match_indices(title) {
            for (i, _) in positions
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, (byte, _))| *byte < start)
                .take(3)
            {
                blocked[i] = true;
            }
        }
    }
    let mut previous = None;
    for edit in edits {
        ensure!(
            edit.start < result.len() && previous.is_none_or(|p| edit.start > p),
            "overlapping or unordered spelling edits"
        );
        let (byte, source) = positions[edit.start];
        ensure!(
            source == edit.source
                && han(source)
                && han(edit.target)
                && source != edit.target
                && !blocked[edit.start]
                && !PROTECTED.contains(edit.target),
            "invalid spelling edit"
        );
        ensure!(
            !crate::code_at(text, byte, byte + source.len_utf8()),
            "code edit rejected"
        );
        result[edit.start] = edit.target;
        previous = Some(edit.start);
    }
    Ok(result.into_iter().collect())
}

fn block_span(blocked: &mut [bool], positions: &[(usize, char)], start: usize, end: usize) {
    for (i, (byte, _)) in positions.iter().enumerate() {
        if *byte >= start && *byte < end {
            blocked[i] = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn edit(text: &str, source: char, target: char) -> Edit {
        Edit {
            start: text.chars().position(|ch| ch == source).unwrap(),
            source,
            target,
        }
    }

    #[test]
    fn character_offsets_preserve_emoji_english_and_all_other_text() {
        let text = "🎙️ GitHub 這次的結過很好。";
        assert_eq!(
            apply(text, &[edit(text, '過', '果')], &[]).unwrap(),
            "🎙️ GitHub 這次的結果很好。"
        );
    }

    #[test]
    fn rejects_numeric_negation_english_and_stale_changes() {
        for (text, source, target) in [
            ("不能刪", '不', '布'),
            ("買三個", '三', '山'),
            ("cat", 'a', 'o'),
            ("布料", '布', '不'),
            ("他做事一向很仔細。", '他', '她'),
            ("我要買機車。", '買', '賣'),
        ] {
            assert!(apply(text, &[edit(text, source, target)], &[]).is_err());
        }
        assert!(apply(
            "今天",
            &[Edit {
                start: 0,
                source: '新',
                target: '心'
            }],
            &[]
        )
        .is_err());
    }

    #[test]
    fn rejects_literals_paths_canonical_terms_and_titled_names() {
        for text in [
            "`新情`",
            "```text\n新情\n```",
            "``新`情",
            "/tmp/今天新情很好.txt",
            "今天新情很好.txt",
            "變數名稱是新情。",
            "「新情」",
            "陳新宇教授",
        ] {
            assert!(
                apply(text, &[edit(text, '新', '心')], &[]).is_err(),
                "{text}"
            );
        }
        let text = "這是新情公司。";
        assert!(apply(text, &[edit(text, '新', '心')], &["新情公司"]).is_err());
        let text = "`新` 今天新情很好。";
        assert_eq!(
            apply(
                text,
                &[Edit {
                    start: 6,
                    source: '新',
                    target: '心'
                }],
                &[]
            )
            .unwrap(),
            "`新` 今天心情很好。"
        );
    }

    #[test]
    fn protects_single_quotes_without_treating_contractions_as_open_quotes() {
        for text in [
            "他說‘今天新情很好。’",
            "他說'今天新情很好。'",
            "'don't 改新情'",
            "‘don’t 改新情’",
        ] {
            assert!(apply(text, &[edit(text, '新', '心')], &[]).is_err());
        }
        for text in [
            "It's 今天新情很好。",
            "James' 今天新情很好。",
            "don’t 今天新情很好。",
        ] {
            assert_eq!(
                apply(text, &[edit(text, '新', '心')], &[]).unwrap(),
                text.replace('新', "心")
            );
        }
    }
}
