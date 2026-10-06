//! The desktop/provider seam: actual daemon NDJSON over an OS socket. Only the
//! microphone and native model boundaries are substituted; no live device opens.
use crate::{
    asr::{Transcriber, Utterance},
    assistant::Assistant,
    audio::capture::{CaptureSource, Recording, RecordingMark, StreamFormat},
    ipc::Server,
    personalization::Personalization,
    postproc::{Traditional, Vocab},
    session::{Pipeline, SessionManager},
};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

struct Microphone;
impl CaptureSource for Microphone {
    fn begin(&self) -> anyhow::Result<RecordingMark> {
        Ok(RecordingMark {
            position: 0,
            format: StreamFormat {
                sample_rate: 16_000,
                channels: 1,
            },
        })
    }
    fn end(&self, _: RecordingMark) -> Recording {
        Recording {
            samples: vec![0.25; 1600],
            sample_rate: 16_000,
        }
    }
    fn cancel(&self) {}
}

struct NativeModel;
impl Transcriber for NativeModel {
    fn transcribe(&self, _: Utterance<'_>) -> anyhow::Result<String> {
        Ok("请检查 GitHub。".into())
    }
    fn name(&self) -> &str {
        "test-native-boundary"
    }
}

async fn read_event(reader: &mut BufReader<UnixStream>) -> serde_json::Value {
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
        .await
        .expect("daemon did not acknowledge lifecycle event")
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn local_provider_acknowledges_capture_and_release_after_the_complete_result() {
    for events in [true, false] {
        let profile = tempfile::tempdir().unwrap();
        let socket = profile.path().join("ipc.sock");
        let server = Server::bind(&socket).unwrap();
        let manager = Arc::new(SessionManager::new(
            Arc::new(Microphone),
            Pipeline {
                asr: Arc::new(NativeModel),
                vad: None,
                traditional: Some(Arc::new(Traditional::load().expect("OpenCC data required"))),
                vocab: Arc::new(Vocab::load_or_empty(&profile.path().join("vocab.toml"))),
                assistant: Arc::new(Assistant::new(Personalization::memory(), None)),
                review: None,
            },
        ));
        let task = tokio::spawn(server.run(manager));
        let mut reader = BufReader::new(UnixStream::connect(&socket).await.unwrap());
        reader
            .get_mut()
            .write_all(if events {
                b"{\"type\":\"start\",\"session\":17,\"session_events\":true}\n"
            } else {
                b"{\"type\":\"start\",\"session\":17}\n"
            })
            .await
            .unwrap();
        if events {
            assert_eq!(
                read_event(&mut reader).await,
                serde_json::json!({"type":"state","session":17,"value":"recording"})
            );
        }
        reader
            .get_mut()
            .write_all(b"{\"type\":\"stop\",\"session\":17}\n")
            .await
            .unwrap();
        assert_eq!(
            read_event(&mut reader).await,
            serde_json::json!({"type":"result","session":17,"text":"請檢查 GitHub。"})
        );
        if events {
            assert_eq!(
                read_event(&mut reader).await,
                serde_json::json!({"type":"state","session":17,"value":"idle"})
            );
        }
        reader
            .get_mut()
            .write_all(b"{\"type\":\"ping\"}\n")
            .await
            .unwrap();
        assert_eq!(
            read_event(&mut reader).await,
            serde_json::json!({"type":"pong"})
        );
        task.abort();
    }
}

#[tokio::test]
async fn cancelled_inference_releases_only_after_native_work_finishes_and_never_returns_text() {
    struct SlowModel {
        started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        finish: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl Transcriber for SlowModel {
        fn transcribe(&self, _: Utterance<'_>) -> anyhow::Result<String> {
            self.started
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(())
                .unwrap();
            self.finish
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))?;
            Ok("這段取消了".into())
        }
        fn name(&self) -> &str {
            "gated-native-boundary"
        }
    }
    let profile = tempfile::tempdir().unwrap();
    let socket = profile.path().join("ipc.sock");
    let server = Server::bind(&socket).unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    let manager = Arc::new(SessionManager::new(
        Arc::new(Microphone),
        Pipeline {
            asr: Arc::new(SlowModel {
                started: std::sync::Mutex::new(Some(started_tx)),
                finish: std::sync::Mutex::new(finish_rx),
            }),
            vad: None,
            traditional: Some(Arc::new(Traditional::load().unwrap())),
            vocab: Arc::new(Vocab::load_or_empty(&profile.path().join("vocab.toml"))),
            assistant: Arc::new(Assistant::new(Personalization::memory(), None)),
            review: None,
        },
    ));
    let task = tokio::spawn(server.run(manager));
    let mut reader = BufReader::new(UnixStream::connect(&socket).await.unwrap());
    reader
        .get_mut()
        .write_all(b"{\"type\":\"start\",\"session\":21,\"session_events\":true}\n")
        .await
        .unwrap();
    assert_eq!(read_event(&mut reader).await["value"], "recording");
    reader
        .get_mut()
        .write_all(b"{\"type\":\"stop\",\"session\":21}\n")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), started_rx)
        .await
        .unwrap()
        .unwrap();
    reader
        .get_mut()
        .write_all(b"{\"type\":\"cancel\",\"session\":21}\n{\"type\":\"ping\"}\n")
        .await
        .unwrap();
    assert_eq!(
        read_event(&mut reader).await,
        serde_json::json!({"type":"pong"})
    );
    let mut premature = String::new();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), reader.read_line(&mut premature))
            .await
            .is_err()
    );
    assert!(premature.is_empty());
    finish_tx.send(()).unwrap();
    assert_eq!(
        read_event(&mut reader).await,
        serde_json::json!({"type":"state","session":21,"value":"idle"})
    );
    reader
        .get_mut()
        .write_all(b"{\"type\":\"ping\"}\n")
        .await
        .unwrap();
    assert_eq!(
        read_event(&mut reader).await,
        serde_json::json!({"type":"pong"})
    );
    task.abort();
}
