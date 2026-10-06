//! Unix domain socket server (SDD §3.3, §4.3)。
//!
//! 選 Unix socket 而非 D-Bus 的理由見 SDD §3.3: 對端 (fcitx5 addon) 可以
//! 用 `EventLoop::addIOEvent` 直接監聽 fd, 不需要額外執行緒也不需要跨
//! 執行緒 dispatch。代價是沒有 introspection/activation, 可接受。

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::protocol::{ClientMessage, ServerMessage};

/// 單則訊息上限, 與 C++ 端的 `kMaxLineBytes` 對齊。
const MAX_LINE_BYTES: usize = 256 * 1024;

/// 出站訊息佇列深度。addon 端消化很快, 塞滿代表對端有問題。
const OUTBOUND_QUEUE: usize = 64;

/// $XDG_RUNTIME_DIR/voicetype/ipc.sock (SDD §4.3, §7)
///
/// 註: SDD §3.1 的架構圖寫 `~/.local/share/voicetype/ipc.sock`, 與 §4.3
/// 不一致。以 §4.3 為準 —— runtime dir 才有正確的生命週期 (登出即清除)
/// 與權限語意 (0700, 僅本使用者)。C++ 端使用相同的解析規則。
pub fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("VOICETYPE_SOCKET") {
        return PathBuf::from(p);
    }
    if let Ok(rt) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(rt).join("voicetype").join("ipc.sock");
    }
    // 沒有 XDG_RUNTIME_DIR 的環境 (少見) 退回 /tmp, 以 uid 隔離。
    PathBuf::from(format!("/tmp/voicetype-{}/ipc.sock", current_uid()))
}

fn current_uid() -> u32 {
    // 避免為了一個 getuid 引入 libc crate。/proc/self 的擁有者就是本行程
    // 的 real uid。
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .unwrap_or(0)
}

/// 送往 addon 的通道。複製成本低, 可以交給各處使用。
#[derive(Clone)]
pub struct Responder {
    tx: mpsc::Sender<ServerMessage>,
}

impl Responder {
    /// Report whether a result entered the bounded outbound queue. This is not
    /// confirmation that the focused application actually inserted the text.
    pub fn try_send(&self, msg: ServerMessage) -> bool {
        self.tx.try_send(msg).is_ok()
    }

    /// 送出一則訊息。連線已斷時靜默丟棄 —— 對端會重連, 此時的訊息
    /// 本來就沒有意義 (session 已經過期)。
    pub fn send(&self, msg: ServerMessage) {
        if let Err(e) = self.tx.try_send(msg) {
            debug!("dropping outbound message: {e}");
        }
    }
}

/// 處理一則來自 addon 的訊息。
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, msg: ClientMessage, responder: Responder);
    /// addon 斷線 (fcitx5 重啟等)。進行中的 session 應該被放棄。
    fn on_disconnect(&self) {}
}

pub struct Server {
    listener: UnixListener,
    path: PathBuf,
}

impl Server {
    pub fn bind(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating socket dir {}", dir.display()))?;
            // $XDG_RUNTIME_DIR 本身已是 0700, 這裡再確保一次。
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("chmod 0700 {}", dir.display()))?;
        }

        // 殘留的 socket 檔案: 先確認是否還有活著的 daemon 佔用。
        // 能連上代表有另一個實例在跑 —— 這種情況要明確失敗, 而不是
        // 把對方的 socket 搶走。
        if path.exists() {
            match std::os::unix::net::UnixStream::connect(path) {
                Ok(_) => {
                    anyhow::bail!(
                        "another voicetyped is already listening on {}",
                        path.display()
                    );
                }
                Err(_) => {
                    debug!("removing stale socket {}", path.display());
                    let _ = std::fs::remove_file(path);
                }
            }
        }

        let listener =
            UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", path.display()))?;

        info!("listening on {}", path.display());
        Ok(Self {
            listener,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 接受連線並處理, 直到被取消。
    ///
    /// 實務上只會有一個 addon 連線 (每個 fcitx5 行程一條), 但重啟 fcitx5
    /// 會產生新連線, 所以不假設單一連線。
    pub async fn run<H: Handler>(self, handler: std::sync::Arc<H>) -> Result<()> {
        loop {
            let (stream, _) = match self.listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    warn!("accept failed: {e}");
                    continue;
                }
            };
            debug!("addon connected");
            let handler = handler.clone();
            tokio::spawn(async move {
                if let Err(e) = serve_connection(stream, handler.clone()).await {
                    debug!("connection closed: {e}");
                }
                debug!("addon disconnected");
            });
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

async fn serve_connection<H: Handler>(
    stream: UnixStream,
    handler: std::sync::Arc<H>,
) -> Result<()> {
    struct DisconnectGuard<H: Handler> {
        handler: std::sync::Arc<H>,
        owns_recording: bool,
    }
    impl<H: Handler> Drop for DisconnectGuard<H> {
        fn drop(&mut self) {
            if self.owns_recording {
                self.handler.on_disconnect();
            }
        }
    }
    let mut guard = DisconnectGuard {
        handler: handler.clone(),
        owns_recording: false,
    };
    let (read_half, mut write_half) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<ServerMessage>(OUTBOUND_QUEUE);
    let responder = Responder { tx };

    // 出站: 獨立 task, 避免序列化/寫入阻塞入站處理。
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let line = match msg.to_line() {
                Ok(l) => l,
                Err(e) => {
                    warn!("failed to serialize outbound message: {e}");
                    continue;
                }
            };
            if write_half.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });

    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break; // EOF
        }
        if n > MAX_LINE_BYTES {
            warn!("oversized message from addon, closing connection");
            break;
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<ClientMessage>(trimmed) {
            Ok(msg) => {
                if matches!(msg, ClientMessage::Start { .. }) {
                    guard.owns_recording = true;
                }
                handler.handle(msg, responder.clone());
            }
            // 無法解析的訊息不該讓連線斷掉 —— 可能只是版本不一致的
            // 新訊息型別, 忽略即可 (協定要能往前相容)。
            Err(e) => warn!("malformed message from addon: {e}"),
        }
    }

    drop(responder);
    let _ = writer.await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::Duration;

    #[derive(Default)]
    struct RecordingHandler {
        messages: AtomicUsize,
        disconnects: AtomicUsize,
    }

    impl Handler for RecordingHandler {
        fn handle(&self, _msg: ClientMessage, responder: Responder) {
            self.messages.fetch_add(1, Ordering::SeqCst);
            responder.send(ServerMessage::Pong);
        }
        fn on_disconnect(&self) {
            self.disconnects.fetch_add(1, Ordering::SeqCst);
        }
    }

    async fn send_and_disconnect(messages: &str) -> Arc<RecordingHandler> {
        let handler = Arc::new(RecordingHandler::default());
        let (server, mut client) = UnixStream::pair().unwrap();
        client.write_all(messages.as_bytes()).await.unwrap();
        client.shutdown().await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(1),
            serve_connection(server, handler.clone()),
        )
        .await
        .expect("connection should complete after EOF")
        .unwrap();
        handler
    }

    #[tokio::test]
    async fn control_only_disconnect_does_not_cancel_another_clients_recording() {
        let handler = send_and_disconnect(concat!(
            "{\"type\":\"ping\"}\n",
            "{\"type\":\"list_learned\"}\n",
            "{\"type\":\"set_context\",\"text\":\"GitHub\"}\n",
            "{\"type\":\"process_text\",\"text\":\"mabe\"}\n",
        ))
        .await;
        assert_eq!(handler.messages.load(Ordering::SeqCst), 4);
        assert_eq!(handler.disconnects.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn recording_connection_disconnect_cancels_its_session() {
        let handler = send_and_disconnect(concat!(
            "{\"type\":\"ping\"}\n",
            "{\"type\":\"start\",\"session\":42,\"program\":\"test\"}\n",
        ))
        .await;
        assert_eq!(handler.messages.load(Ordering::SeqCst), 2);
        assert_eq!(handler.disconnects.load(Ordering::SeqCst), 1);
    }
}
