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

const MAX_REPLY: usize = 32 * 1024;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub struct CscClient {
    path: PathBuf,
    timeout: Duration,
    owned: Option<voicetype_text::spelling::OwnedSpelling>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    v: u32,
    id: u64,
    status: String,
    model_calls: u32,
    edits: Vec<serde_json::Value>,
}

impl CscClient {
    pub fn owned_healthy(&self) -> bool {
        self.owned.as_ref().is_some_and(voicetype_text::spelling::OwnedSpelling::healthy)
    }

    pub fn from_env() -> Result<Option<Self>> {
        if let Some(config) = std::env::var_os("VOICETYPE_CSC_PROCESS") {
            let config = config
                .to_str()
                .context("invalid CPU spelling configuration")?;
            let paths: voicetype_text::spelling::SpellingPaths = serde_json::from_str(config)
                .map_err(|_| anyhow::anyhow!("invalid CPU spelling configuration"))?;
            let worker = voicetype_text::spelling::OwnedSpelling::start(
                paths.command()?,
                Duration::from_secs(30),
            )?;
            return Ok(Some(Self {
                path: PathBuf::new(),
                timeout: Duration::from_millis(100),
                owned: Some(worker),
            }));
        }
        let Some(path) = std::env::var_os("VOICETYPE_CSC_SOCKET") else {
            return Ok(None);
        };
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            tracing::warn!("CPU spelling disabled: socket path must be absolute");
            return Ok(None);
        }
        Ok(Some(Self {
            path,
            timeout: Duration::from_millis(100),
            owned: None,
        }))
    }

    pub fn correct(&self, text: &str, terms: &[String]) -> String {
        if let Some(worker) = &self.owned {
            let started = Instant::now();
            let result = worker.correct(text, terms);
            tracing::info!(
                changed = result != text,
                elapsed_us = started.elapsed().as_micros() as u64,
                "owned CPU spelling completed; original retained when unavailable"
            );
            return result;
        }
        if !voicetype_text::spelling_policy::accepts(text) {
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
        let output =
            voicetype_text::spelling_policy::apply_reply(text, &terms, id, &data[..frame])?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::thread;

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
            owned: None,
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
            owned: None,
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
                owned: None,
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
            owned: None,
        };
        assert_eq!(client.correct("今天新情很好。", &[]), "今天新情很好。");
        let long = "新".repeat(1025);
        assert_eq!(client.correct(&long, &[]), long);
    }
}
