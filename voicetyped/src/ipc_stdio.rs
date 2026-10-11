//! One inherited, private stdio connection for the Windows app-owned engine.
//! No TCP listener, pipe name, or ambient client can issue recording commands.
use anyhow::Result;
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, sync::mpsc};
use tracing::{debug, warn};
use crate::protocol::{ClientMessage, ServerMessage};

const MAX_LINE_BYTES: usize = 256 * 1024;
const OUTBOUND_QUEUE: usize = 64;

#[derive(Clone)]
pub struct Responder { tx: mpsc::Sender<ServerMessage> }
impl Responder {
    pub fn try_send(&self, msg: ServerMessage) -> bool { self.tx.try_send(msg).is_ok() }
    pub fn send(&self, msg: ServerMessage) { let _ = self.try_send(msg); }
}
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, msg: ClientMessage, responder: Responder);
    fn on_disconnect(&self) {}
}

pub async fn run<H: Handler>(handler: std::sync::Arc<H>) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<ServerMessage>(OUTBOUND_QUEUE);
    let responder = Responder { tx };
    let mut stdout = tokio::io::stdout();
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let line = msg.to_line()?;
            stdout.write_all(line.as_bytes()).await?;
            stdout.flush().await?;
        }
        Ok::<(), anyhow::Error>(())
    });
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut line = Vec::new();
    let mut owns_recording = false;
    loop {
        line.clear();
        let mut eof = false;
        loop {
            let available = stdin.fill_buf().await?;
            if available.is_empty() { eof = true; break; }
            let count = available.iter().position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            if line.len() + count > MAX_LINE_BYTES {
                warn!("oversized stdio frame; closing engine connection");
                eof = true;
                break;
            }
            line.extend_from_slice(&available[..count]);
            stdin.consume(count);
            if line.ends_with(b"\n") { break; }
        }
        if eof && line.is_empty() { break; }
        if eof || !line.ends_with(b"\n") {
            warn!("invalid stdio frame; closing engine connection");
            break;
        }
        match serde_json::from_slice::<ClientMessage>(&line) {
            Ok(msg) => {
                if matches!(msg, ClientMessage::Start { .. }) { owns_recording = true; }
                handler.handle(msg, responder.clone());
            }
            Err(_) => debug!("discarding malformed desktop command"),
        }
    }
    if owns_recording { handler.on_disconnect(); }
    drop(responder);
    writer.abort();
    Ok(())
}
