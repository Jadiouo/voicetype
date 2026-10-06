//! Optional CPU character correction over a private Unix socket.
//! The original transcript stays here; the worker only returns positional edits.
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::json;
use socket2::{Domain, SockAddr, Socket, Type};

const MAX_TEXT: usize = 1024;
const MAX_REPLY: usize = 32 * 1024;
const PROTECTED: &str = "不沒没未無无非勿莫別别零〇一二兩两三四五六七八九十百千萬万億亿兆幾几我你妳您他她它牠祂咱俺買买賣卖";
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub struct CscClient {
    path: PathBuf,
    timeout: Duration,
}

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

impl CscClient {
    pub fn from_env() -> Option<Self> {
        let path = PathBuf::from(std::env::var_os("VOICETYPE_CSC_SOCKET")?);
        if !path.is_absolute() {
            tracing::warn!("CPU spelling disabled: socket path must be absolute");
            return None;
        }
        Some(Self {
            path,
            timeout: Duration::from_millis(100),
        })
    }

    pub fn correct(&self, text: &str, terms: &[String]) -> String {
        if text.chars().count() > MAX_TEXT || !text.chars().any(han) {
            return text.to_owned();
        }
        let started = Instant::now();
        match self.request(text, terms, started + self.timeout) {
            Ok((result, reply)) => {
                tracing::info!(
                    status = reply.status,
                    edits = reply.edits.len(),
                    model_calls = reply.model_calls,
                    elapsed_us = started.elapsed().as_micros() as u64,
                    "CPU spelling correction completed"
                );
                result
            }
            Err(_) => {
                // Do not log request content or a parser error containing text.
                tracing::info!(
                    elapsed_us = started.elapsed().as_micros() as u64,
                    "CPU spelling unavailable or rejected; original retained"
                );
                text.to_owned()
            }
        }
    }

    fn request(&self, text: &str, terms: &[String], deadline: Instant) -> Result<(String, Reply)> {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let terms: Vec<&str> = terms
            .iter()
            .map(String::as_str)
            .filter(|term| !term.is_empty() && term.chars().count() <= 64 && text.contains(term))
            .take(256)
            .collect();
        let mut request = serde_json::to_vec(&json!({
            "v": 1, "id": id, "text": text, "terms": terms,
            "sent_at_ms": SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64,
        }))?;
        request.push(b'\n');
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        socket.connect_timeout(&SockAddr::unix(&self.path)?, remaining(deadline)?)?;
        let mut stream: UnixStream = socket.into();
        let mut sent = 0;
        while sent < request.len() {
            stream.set_write_timeout(Some(remaining(deadline)?))?;
            let n = stream.write(&request[sent..])?;
            ensure!(n != 0, "spelling worker disconnected");
            sent += n;
        }
        let mut data = Vec::new();
        let mut buf = [0; 4096];
        let frame = loop {
            stream.set_read_timeout(Some(remaining(deadline)?))?;
            let n = stream.read(&mut buf)?;
            ensure!(
                n != 0 && data.len() + n <= MAX_REPLY,
                "invalid spelling response length"
            );
            data.extend_from_slice(&buf[..n]);
            if let Some(end) = data.iter().position(|b| *b == b'\n') {
                break end;
            }
        };
        let reply: Reply = serde_json::from_slice(&data[..frame])?;
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
        let output = apply(text, &reply.edits, &terms)?;
        remaining(deadline)?;
        Ok((output, reply))
    }
}

fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .context("spelling deadline exceeded")
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
            !crate::refine::code_at(text, byte, byte + source.len_utf8()),
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
    use std::os::unix::net::UnixListener;
    use std::thread;

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

    fn server(
        handler: impl FnOnce(UnixStream) + Send + 'static,
    ) -> (PathBuf, thread::JoinHandle<()>) {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("voicetype-csc-{}-{id}.sock", std::process::id()));
        let listener = UnixListener::bind(&path).unwrap();
        let cleanup = path.clone();
        let worker = thread::spawn(move || {
            handler(listener.accept().unwrap().0);
            std::fs::remove_file(cleanup).unwrap();
        });
        (path, worker)
    }

    fn request(stream: &mut UnixStream) -> serde_json::Value {
        let mut bytes = Vec::new();
        let mut byte = [0];
        loop {
            stream.read_exact(&mut byte).unwrap();
            if byte[0] == b'\n' {
                break;
            }
            bytes.push(byte[0]);
        }
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn applies_a_valid_private_worker_response() {
        let (path, worker) = server(|mut stream| {
            let request = request(&mut stream);
            let response = json!({"v":1,"id":request["id"],"status":"applied","model_calls":1,
                "edits":[{"start":2,"source":"新","target":"心"}]})
            .to_string()
                + "\n";
            stream.write_all(response.as_bytes()).unwrap();
        });
        let client = CscClient {
            path,
            timeout: Duration::from_millis(100),
        };
        assert_eq!(client.correct("今天新情很好。", &[]), "今天心情很好。");
        worker.join().unwrap();
    }

    #[test]
    fn slow_worker_is_bounded_and_keeps_original() {
        let (path, worker) = server(|mut stream| {
            request(&mut stream);
            thread::sleep(Duration::from_millis(150));
        });
        let client = CscClient {
            path,
            timeout: Duration::from_millis(25),
        };
        let start = Instant::now();
        assert_eq!(client.correct("今天新情很好。", &[]), "今天新情很好。");
        assert!(start.elapsed() < Duration::from_millis(120));
        worker.join().unwrap();
    }

    #[test]
    fn malformed_or_mismatched_responses_keep_original() {
        for response in [
            "{bad json}\n",
            "{\"v\":1,\"id\":0,\"status\":\"applied\",\"model_calls\":1,\"edits\":[]}\n",
        ] {
            let (path, worker) = server(move |mut stream| {
                request(&mut stream);
                stream.write_all(response.as_bytes()).unwrap();
            });
            let client = CscClient {
                path,
                timeout: Duration::from_millis(100),
            };
            assert_eq!(client.correct("今天新情很好。", &[]), "今天新情很好。");
            worker.join().unwrap();
        }
    }

    #[test]
    fn absent_worker_and_oversize_input_keep_every_character() {
        let client = CscClient {
            path: PathBuf::from("/nonexistent/csc.sock"),
            timeout: Duration::from_millis(100),
        };
        assert_eq!(client.correct("今天新情很好。", &[]), "今天新情很好。");
        let long = "新".repeat(1025);
        assert_eq!(client.correct(&long, &[]), long);
    }
}
