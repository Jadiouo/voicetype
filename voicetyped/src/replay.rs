//! Isolated acceptance seam. No microphone, model, socket, refiner or desktop
//! helper is constructed. References arrive only in later Correction messages.
use std::collections::HashSet;
use std::io::{BufRead, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};

use crate::assistant::Assistant;
use crate::personalization::{ContextSnapshot, Personalization};
use crate::postproc::{Traditional, Vocab};
use crate::protocol::ClientMessage;

pub fn requested() -> Result<Option<String>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !args.iter().any(|a| a == "--replay-jsonl") {
        return Ok(None);
    }
    ensure!(
        args.len() == 2 && args[0] == "--replay-jsonl",
        "usage: --replay-jsonl <file|->"
    );
    Ok(Some(args[1].clone()))
}

pub fn run(input: &str) -> Result<()> {
    let path = std::env::var_os("VOICETYPE_LEARNING_FILE")
        .map(std::path::PathBuf::from)
        .context("replay requires a new isolated VOICETYPE_LEARNING_FILE")?;
    validate_store(&path)?;
    // Create-new makes the empty-store requirement atomic, including symlinks.
    // A nonempty valid JSON store is deliberately NOT copied from production.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(b"{\"version\":1,\"rules\":[]}")?;
    file.sync_all()?;
    drop(file);
    let assistant = Assistant::new(Personalization::load(&path)?, None);
    let traditional = Traditional::load().context("OpenCC s2tw data required for replay")?;
    let vocab = Vocab::load_or_empty(&crate::vocab_path());
    let input: Box<dyn BufRead> = if input == "-" {
        Box::new(std::io::BufReader::new(std::io::stdin()))
    } else {
        Box::new(std::io::BufReader::new(std::fs::File::open(input)?))
    };
    run_lines(
        input,
        std::io::stdout().lock(),
        &traditional,
        &vocab,
        &assistant,
    )
}

fn validate_store(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "replay learning file must be absolute");
    ensure!(
        std::fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
        "replay requires a nonexistent learning file"
    );
    let parent = path
        .parent()
        .context("missing replay store parent")?
        .canonicalize()?;
    let target = parent.join(path.file_name().context("missing replay store filename")?);
    if let Ok(default) = Personalization::default_path() {
        let normalized = default
            .parent()
            .and_then(|p| p.canonicalize().ok())
            .map(|p| p.join(default.file_name().expect("default filename")))
            .unwrap_or(default);
        ensure!(
            target != normalized,
            "replay must not use the production learning file"
        );
    }
    Ok(())
}

fn run_lines(
    mut input: impl BufRead,
    mut output: impl Write,
    traditional: &Traditional,
    vocab: &Vocab,
    assistant: &Assistant,
) -> Result<()> {
    let mut sessions = HashSet::new();
    let mut line_number = 0u64;
    loop {
        // Bounded allocation even for a malformed stream without a newline.
        let mut line = Vec::new();
        let count = input.by_ref().take(65_537).read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        line_number += 1;
        let oversized = line.len() > 65_536;
        if oversized && !line.ends_with(b"\n") {
            loop {
                let bytes = input.fill_buf()?;
                if bytes.is_empty() {
                    break;
                }
                let newline = bytes.iter().position(|b| *b == b'\n');
                let count = newline.map_or(bytes.len(), |i| i + 1);
                input.consume(count);
                if newline.is_some() {
                    break;
                }
            }
        }
        let result = if oversized {
            Err(anyhow::anyhow!("replay line exceeds 65536 bytes"))
        } else {
            process_line(&line, traditional, vocab, assistant, &mut sessions)
        };
        let response = result.unwrap_or_else(
            |error| json!({"type":"replay_error", "line":line_number, "text":error.to_string()}),
        );
        serde_json::to_writer(&mut output, &response)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}

fn process_line(
    line: &[u8],
    traditional: &Traditional,
    vocab: &Vocab,
    assistant: &Assistant,
    sessions: &mut HashSet<u64>,
) -> Result<Value> {
    let value: Value = serde_json::from_slice(line).context("invalid replay JSON")?;
    let message: ClientMessage =
        serde_json::from_value(value.clone()).context("invalid replay message")?;
    match message {
        ClientMessage::ProcessText {
            text,
            context_text,
            program,
            context_id,
            selected_text,
            mode,
        } => {
            let session = value["replay_session"]
                .as_u64()
                .context("process_text requires replay_session u64")?;
            ensure!(!sessions.contains(&session), "duplicate replay session");
            ensure!(sessions.len() < 100_000, "too many replay sessions");
            let scope = ContextSnapshot {
                program,
                context_id,
                text: context_text,
                selected_text,
            }
            .bounded();
            let text = crate::output::process(
                Some(traditional),
                vocab,
                assistant,
                &text,
                &scope,
                mode.as_deref(),
            )?;
            assistant.remember(session, scope, text.clone());
            sessions.insert(session);
            Ok(json!({"type":"replay_result", "session":session, "text":text}))
        }
        ClientMessage::Correction {
            session,
            program,
            context_id,
            before,
            after,
            confirmed,
        } => {
            let outcome = assistant.correction_detailed(
                session,
                &program,
                &context_id,
                &before,
                &after,
                confirmed,
            )?;
            Ok(json!({"type":"replay_correction", "outcome":outcome}))
        }
        ClientMessage::ListLearned => Ok(json!({"type":"replay_info", "value":assistant.list()?})),
        _ => bail!("replay accepts only process_text, correction and list_learned"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_scores_before_feedback_and_keeps_attribution_guards() {
        let traditional = Traditional::load().expect("OpenCC data required");
        let vocab = Vocab::load_or_empty(Path::new("/nonexistent-voicetype-test/vocab.toml"));
        let assistant = Assistant::new(Personalization::memory(), None);
        let mut lines = Vec::new();
        for value in [
            json!({"type":"process_text","replay_session":1,"text":"推到gthub上面","program":"editor","context_id":"a","mode":"off"}),
            json!({"type":"correction","session":1,"program":"editor","context_id":"wrong","before":"推到gthub上面","after":"推到GitHub上面","confirmed":true}),
            json!({"type":"correction","session":1,"program":"editor","context_id":"a","before":"推到gthub上面","after":"推到GitHub上面","confirmed":true}),
            json!({"type":"process_text","replay_session":2,"text":"推到gthub上面","program":"editor","context_id":"a","mode":"off"}),
            json!({"type":"process_text","replay_session":2,"text":"推到gthub上面","program":"editor","context_id":"a"}),
            json!({"type":"list_learned"}),
        ] {
            serde_json::to_writer(&mut lines, &value).unwrap();
            lines.push(b'\n');
        }
        let mut out = Vec::new();
        run_lines(lines.as_slice(), &mut out, &traditional, &vocab, &assistant).unwrap();
        let got: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(got[0]["text"], "推到gthub上面");
        assert_eq!(got[1]["type"], "replay_error");
        assert_eq!(got[2]["outcome"]["status"], "confirmed");
        assert_eq!(got[3]["text"], "推到GitHub上面");
        assert_eq!(got[4]["type"], "replay_error");
        assert_eq!(got[5]["type"], "replay_info");
    }
}
