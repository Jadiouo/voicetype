//! Session 狀態機 (SDD §3.1)。
//!
//! ```text
//! Idle → Recording → Transcribing → Delivering
//! ```
//!
//! 併發模型: 所有 session 命令由**單一** task 依序處理, 順序因此天然
//! 成立 —— 不需要為 start/stop 的交錯設計鎖。真正耗時的兩件事各自
//! 離開這條序列:
//!
//! * 音訊操作 (開串流、取資料) 走 `spawn_blocking` 並 `await`, 仍在序列內;
//! * ASR 推論 (250–300ms) 丟到獨立 task, **不** await —— 否則使用者在
//!   推論期間按下的下一次 PTT 會被卡住。
//!
//! 代價是結果可能在下一個 session 已經開始後才回來。這是 SDD §4.2.5
//! 「過期結果」的來源, 兩端都要防守: daemon 送出前比對 `latest`,
//! addon 收到後也比對自己的 sessionId。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::asr::{Transcriber, Utterance};
use crate::audio::capture::{exceeds_max_duration, RecordingMark};
use crate::audio::{resample, AudioCapture, MAX_RECORDING_SECONDS};
use crate::ipc::{Handler, Responder};
use crate::postproc::{strip_tags, Traditional, Vocab};
use crate::protocol::{ClientMessage, ErrorCode, ServerMessage};
use crate::vad::{self, SpeechDetector, VadConfig};

/// 從音訊到可 commit 文字的三個階段 (SDD §4.4 / §4.1 / §4.6)。
///
/// 包成一個 struct 而不是三個參數: 後處理還有 ③–⑥ 沒做, 每加一階
/// 就改一次 `SessionManager::new` 的簽名不划算。
///
/// 兩個 `Option` 的語意不同。`vad` 為 `None` 是建置沒有引擎 (VAD 權重
/// 在同一個 GGUF 裡); `traditional` 為 `None` 是系統缺 OpenCC 資料檔,
/// 此時輸出簡體而不是讓整個聽寫不能用 (理由見 `postproc::traditional`)。
pub struct Pipeline {
    pub asr: Arc<dyn Transcriber>,
    pub vad: Option<Arc<dyn SpeechDetector>>,
    pub traditional: Option<Arc<Traditional>>,
    /// §4.6 ③。空表等於這一階不做事, 不需要 Option。
    pub vocab: Arc<Vocab>,
}

/// 內部命令。比 `ClientMessage` 多一個逾時, 少一個 ping (ping 不需要
/// 進到序列裡)。
enum Command {
    Start {
        session: u64,
        program: String,
        responder: Responder,
    },
    Stop {
        session: u64,
        responder: Responder,
    },
    Cancel {
        session: u64,
    },
    /// 錄音超過上限 (SDD §4.4)。
    Timeout {
        session: u64,
        responder: Responder,
    },
    Fallback {
        text: String,
        responder: Responder,
    },
    /// addon 斷線: 放棄進行中的錄音。
    Disconnect,
}

struct Active {
    session: u64,
    program: String,
    mark: RecordingMark,
    started: Instant,
}

pub struct SessionManager {
    tx: mpsc::UnboundedSender<Command>,
    /// 最新的 session id。ASR 完成時用來判斷結果是否已過期。
    latest: Arc<AtomicU64>,
}

impl SessionManager {
    pub fn new(audio: Arc<AudioCapture>, pipeline: Pipeline) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let latest = Arc::new(AtomicU64::new(0));
        // 迴圈自己也需要 sender: 錄音上限的計時器要把 Timeout 命令送回
        // 同一條序列, 才能安全地與 stop/cancel 競爭。
        tokio::spawn(run(rx, tx.clone(), audio, Arc::new(pipeline), latest.clone()));
        Self { tx, latest }
    }
}

impl Handler for SessionManager {
    fn handle(&self, msg: ClientMessage, responder: Responder) {
        let cmd = match msg {
            ClientMessage::Ping => {
                // 不進序列: ping 只是探活, 不該排在錄音後面。
                responder.send(ServerMessage::Pong);
                return;
            }
            ClientMessage::Start {
                session,
                program,
                is_password,
            } => {
                // 縱深防禦 (SDD §7)。addon 已經在密碼欄位拒絕啟動,
                // 走到這裡代表 addon 版本不符或協定被繞過。
                if is_password {
                    warn!(session, "refusing to record: password field");
                    responder.send(ServerMessage::error(
                        Some(session),
                        ErrorCode::Internal,
                        "refusing to record in a password field",
                    ));
                    return;
                }
                self.latest.store(session, Ordering::SeqCst);
                Command::Start {
                    session,
                    program,
                    responder,
                }
            }
            ClientMessage::Stop { session } => Command::Stop { session, responder },
            ClientMessage::Cancel { session } => Command::Cancel { session },
            ClientMessage::FallbackClipboard { text } => Command::Fallback { text, responder },
        };
        let _ = self.tx.send(cmd);
    }

    fn on_disconnect(&self) {
        let _ = self.tx.send(Command::Disconnect);
    }
}

async fn run(
    mut rx: mpsc::UnboundedReceiver<Command>,
    self_tx: mpsc::UnboundedSender<Command>,
    audio: Arc<AudioCapture>,
    pipeline: Arc<Pipeline>,
    latest: Arc<AtomicU64>,
) {
    let mut active: Option<Active> = None;

    while let Some(cmd) = rx.recv().await {
        match cmd {
            Command::Start {
                session,
                program,
                responder,
            } => {
                if let Some(prev) = active.take() {
                    // 上一段錄音沒有正常結束 (例如 addon 漏送 stop)。
                    debug!(prev = prev.session, "abandoning previous recording");
                    let a = audio.clone();
                    let _ = tokio::task::spawn_blocking(move || a.cancel()).await;
                }

                let a = audio.clone();
                let mark = match tokio::task::spawn_blocking(move || a.begin()).await {
                    Ok(Ok(m)) => m,
                    Ok(Err(e)) => {
                        warn!(session, "failed to start capture: {e}");
                        responder.send(ServerMessage::error(
                            Some(session),
                            ErrorCode::NoAudioDevice,
                            format!("{e}"),
                        ));
                        continue;
                    }
                    Err(e) => {
                        warn!(session, "capture task panicked: {e}");
                        responder.send(ServerMessage::error(
                            Some(session),
                            ErrorCode::Internal,
                            "audio thread failed",
                        ));
                        continue;
                    }
                };

                debug!(session, program = %program, "recording");
                active = Some(Active {
                    session,
                    program,
                    mark,
                    started: Instant::now(),
                });

                // 上限計時 (SDD §4.4): 超過上限自動停止並回 too_long,
                // 而不是等使用者放開熱鍵才發現。
                let tx = self_tx.clone();
                let r = responder.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs_f32(
                        MAX_RECORDING_SECONDS,
                    ))
                    .await;
                    let _ = tx.send(Command::Timeout {
                        session,
                        responder: r,
                    });
                });
            }

            Command::Stop { session, responder } => {
                let Some(current) = active.take() else {
                    debug!(session, "stop without an active recording");
                    continue;
                };
                if current.session != session {
                    debug!(
                        session,
                        active = current.session,
                        "stop for a different session, ignoring"
                    );
                    active = Some(current);
                    continue;
                }
                finish(&audio, &pipeline, &latest, current, responder).await;
            }

            Command::Timeout { session, responder } => {
                let Some(current) = active.take() else { continue };
                if current.session != session {
                    active = Some(current);
                    continue;
                }
                warn!(session, "recording exceeded {MAX_RECORDING_SECONDS}s");
                let a = audio.clone();
                let mark = current.mark;
                let _ = tokio::task::spawn_blocking(move || a.end(mark)).await;
                responder.send(ServerMessage::error(
                    Some(session),
                    ErrorCode::TooLong,
                    format!("recording exceeded {MAX_RECORDING_SECONDS} seconds"),
                ));
            }

            Command::Cancel { session } => {
                let Some(current) = active.take() else { continue };
                if current.session != session {
                    active = Some(current);
                    continue;
                }
                debug!(session, "cancelled");
                let a = audio.clone();
                let _ = tokio::task::spawn_blocking(move || a.cancel()).await;
            }

            Command::Fallback { text, responder } => {
                deliver_fallback(&text, &responder);
            }

            Command::Disconnect => {
                if let Some(current) = active.take() {
                    debug!(session = current.session, "addon disconnected mid-recording");
                    let a = audio.clone();
                    let _ = tokio::task::spawn_blocking(move || a.cancel()).await;
                }
            }
        }
    }
}

/// 把配置器的空閒頁還給 OS。
///
/// glibc 的 malloc 在 free 之後通常**不**歸還 —— 它假設程式很快會再要。
/// 對 PTT 這種「每次爆發配置幾十 MB, 然後閒置好幾分鐘」的模式, 那個
/// 假設是錯的: 一次 60 秒錄音的重採樣緩衝加上推論的計算緩衝, 尖峰過後
/// 全部變成常駐。實測 daemon 用一陣子後 RSS 到 494MB, 而同樣的推論在
/// 單行程裡跑 12 次穩定在 378MB —— 差額就是這些沒還回去的頁。
///
/// 呼叫時機是推論**結束後**, 不在熱路徑上。
#[cfg(target_env = "gnu")]
fn release_free_pages() {
    extern "C" {
        fn malloc_trim(pad: usize) -> std::os::raw::c_int;
    }
    // SAFETY: malloc_trim 只操作配置器自己的空閒鏈, 不碰使用中的記憶體。
    unsafe { malloc_trim(0) };
}

#[cfg(not(target_env = "gnu"))]
fn release_free_pages() {}

/// 推論管線的結果。`NoSpeech` 與「引擎回空字串」是不同的事:
/// 前者代表音訊裡根本沒有語音, 引擎連跑都不該跑。
enum Outcome {
    Text(String),
    NoSpeech,
}

/// 取出錄音, 送去轉錄。轉錄本身不 await (見模組說明)。
async fn finish(
    audio: &Arc<AudioCapture>,
    pipeline: &Arc<Pipeline>,
    latest: &Arc<AtomicU64>,
    current: Active,
    responder: Responder,
) {
    let session = current.session;
    let elapsed = current.started.elapsed();

    let a = audio.clone();
    let mark = current.mark;
    let recording = match tokio::task::spawn_blocking(move || a.end(mark)).await {
        Ok(r) => r,
        Err(e) => {
            warn!(session, "capture task panicked: {e}");
            responder.send(ServerMessage::error(
                Some(session),
                ErrorCode::Internal,
                "audio thread failed",
            ));
            return;
        }
    };

    if exceeds_max_duration(&recording) {
        responder.send(ServerMessage::error(
            Some(session),
            ErrorCode::TooLong,
            format!("recording exceeded {MAX_RECORDING_SECONDS} seconds"),
        ));
        return;
    }
    if recording.samples.is_empty() {
        responder.send(ServerMessage::error(
            Some(session),
            ErrorCode::EmptyResult,
            "no audio captured",
        ));
        return;
    }

    debug!(
        session,
        held_ms = elapsed.as_millis() as u64,
        captured_s = recording.duration_secs(),
        program = %current.program,
        "transcribing"
    );

    let pipeline = pipeline.clone();
    // 後處理在推論之後才用得到, 而 pipeline 整個被 move 進 blocking
    // closure。只取這一個 Arc 出來, 比把整包 clone 兩份精準。
    let traditional = pipeline.traditional.clone();
    let vocab = pipeline.vocab.clone();
    let latest = latest.clone();
    tokio::spawn(async move {
        let started = Instant::now();
        let result = tokio::task::spawn_blocking(move || {
            let mut samples = resample::to_target_rate(&recording.samples, recording.sample_rate)?;

            if let Some(detector) = pipeline.vad.as_deref() {
                let t = Instant::now();
                match vad::trim(detector, &samples, &VadConfig::default()) {
                    Ok(Some(speech)) => {
                        debug!(
                            session,
                            vad_ms = t.elapsed().as_millis() as u64,
                            before = samples.len(),
                            after = speech.len(),
                            "trimmed silence"
                        );
                        // truncate + drain 而不是 to_vec: 少一次 3.8MB
                        // (60 秒上限) 的配置。
                        samples.truncate(speech.end);
                        samples.drain(..speech.start);
                    }
                    Ok(None) => {
                        debug!(
                            session,
                            vad_ms = t.elapsed().as_millis() as u64,
                            "no speech in recording"
                        );
                        return Ok(Outcome::NoSpeech);
                    }
                    // VAD 掛掉不該讓聽寫整個失敗 —— 未修剪的音訊仍然
                    // 是可辨識的, 只是失去空錄音防護。
                    Err(e) => warn!(session, "VAD failed, transcribing untrimmed: {e}"),
                }
            }

            pipeline.asr.transcribe(Utterance {
                samples: &samples,
                language: None,
            })
            .map(Outcome::Text)
        })
        .await;

        // 推論的尖峰配置 (重採樣緩衝 + 計算緩衝) 已經 free 掉了,
        // 這裡把頁面真的還給 OS。放在過期檢查之前 —— 就算結果要丟掉,
        // 記憶體還是得還。
        release_free_pages();

        // 過期檢查: 使用者已經開始下一次錄音, 這個結果不該送出。
        if latest.load(Ordering::SeqCst) != session {
            debug!(session, "discarding stale transcription");
            return;
        }

        match result {
            Ok(Ok(Outcome::NoSpeech)) => {
                responder.send(ServerMessage::error(
                    Some(session),
                    ErrorCode::EmptyResult,
                    "no speech detected",
                ));
            }
            Ok(Ok(Outcome::Text(raw))) => {
                let stripped = strip_tags(&raw);
                if stripped.is_no_speech() {
                    responder.send(ServerMessage::error(
                        Some(session),
                        ErrorCode::EmptyResult,
                        "no speech detected",
                    ));
                    return;
                }
                // §4.6 ②: 引擎輸出簡體, §1.1 要繁體台灣用語。
                // 幾十微秒的字串操作, 不值得再繞一次 spawn_blocking。
                let text = match traditional.as_deref() {
                    Some(t) => t.convert(&stripped.text),
                    None => stripped.text,
                };
                // §4.6 ③ 在 ② 之後: 修正表寫的是繁體 (「熱力瑞」),
                // 在繁化之前比對就得為簡繁各寫一份。
                let text = vocab.apply(&text);
                info!(
                    session,
                    latency_ms = started.elapsed().as_millis() as u64,
                    "transcribed"
                );
                responder.send(ServerMessage::Result { session, text });
            }
            Ok(Err(e)) => {
                warn!(session, "transcription failed: {e}");
                responder.send(ServerMessage::error(
                    Some(session),
                    ErrorCode::Internal,
                    format!("{e}"),
                ));
            }
            Err(e) => {
                warn!(session, "transcription task panicked: {e}");
                responder.send(ServerMessage::error(
                    Some(session),
                    ErrorCode::Internal,
                    "transcription failed",
                ));
            }
        }
    });
}

/// 降級鏈③ (SDD §4.8): 寫入檔案並記錄。
///
/// 降級鏈② (wl-copy 進剪貼簿 + desktop notification) 屬於 M3。
fn deliver_fallback(text: &str, responder: &Responder) {
    let path = match dirs_data_home() {
        Some(d) => d.join("voicetype").join("last.txt"),
        None => {
            responder.send(ServerMessage::error(
                None,
                ErrorCode::Internal,
                "no data directory",
            ));
            return;
        }
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, text) {
        Ok(()) => info!("target window disappeared; wrote transcript to {}", path.display()),
        Err(e) => {
            warn!("fallback write failed: {e}");
            responder.send(ServerMessage::error(
                None,
                ErrorCode::Internal,
                format!("fallback write failed: {e}"),
            ));
        }
    }
}

fn dirs_data_home() -> Option<std::path::PathBuf> {
    if let Ok(d) = std::env::var("XDG_DATA_HOME") {
        if !d.is_empty() {
            return Some(std::path::PathBuf::from(d));
        }
    }
    std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".local").join("share"))
}
