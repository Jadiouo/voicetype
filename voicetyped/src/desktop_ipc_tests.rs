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
    fn suspend(&self) -> anyhow::Result<()> {
        Ok(())
    }
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
async fn application_records_through_the_real_local_transport_and_delivers_the_complete_result() {
    use voicetype_app_core::{
        Application, DeliveryOutcome, DeliveryPort, ProviderEvent, TargetLease,
    };
    struct FocusedInput(Vec<String>);
    impl DeliveryPort for FocusedInput {
        fn commit_if_focused(&mut self, target: &TargetLease, text: &str) -> DeliveryOutcome {
            assert_eq!(target, &TargetLease::new("editor", 1));
            self.0.push(text.into());
            DeliveryOutcome::Delivered
        }
    }
    let profile = tempfile::tempdir().unwrap();
    let socket = profile.path().join("ipc.sock");
    let server = Server::bind(&socket).unwrap();
    let manager = Arc::new(SessionManager::new(
        Arc::new(Microphone),
        Pipeline {
            asr: Arc::new(NativeModel),
            vad: None,
            traditional: Some(Arc::new(Traditional::load().unwrap())),
            vocab: Arc::new(Vocab::load_or_empty(&profile.path().join("vocab.toml"))),
            assistant: Arc::new(Assistant::new(Personalization::memory(), None)),
            review: None,
        },
    ));
    let server_task = tokio::spawn(server.run(manager));
    let config = profile.path().join("app");
    tokio::task::spawn_blocking(move || {
        let mut connection = voicetype_app_core::local::LocalConnection::connect_to_process(
            &socket,
            std::process::id(),
            Duration::from_secs(1),
        )
        .unwrap();
        let mut app = Application::open(&config).unwrap();
        let mut output = FocusedInput(vec![]);
        let key = app
            .start_dictation(TargetLease::new("editor", 1), &mut connection)
            .unwrap();
        let (received_key, event) = connection
            .poll_event(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_eq!(received_key, key);
        assert!(matches!(event, ProviderEvent::Recording));
        app.provider_event(received_key, event, &mut output);
        app.stop_dictation(&mut connection).unwrap();
        while app.snapshot().dictation.busy {
            let (received_key, event) = connection
                .poll_event(Duration::from_secs(1))
                .unwrap()
                .unwrap();
            app.provider_event(received_key, event, &mut output);
        }
        assert_eq!(output.0, vec!["請檢查 GitHub。"]);
        assert!(app.snapshot().dictation.failure.is_none());
        connection.suspend(Duration::from_secs(1)).unwrap();
    })
    .await
    .unwrap();
    server_task.abort();
}

#[tokio::test]
async fn desktop_status_tracks_owned_csc_failure_while_asr_keeps_responding() {
    if let Some(scenario) = std::env::var_os("VOICETYPE_TEST_CSC_IPC_CHILD") {
        let profile = tempfile::tempdir().unwrap();
        let socket = profile.path().join("ipc.sock");
        let assistant = Arc::new(Assistant::load().unwrap());
        let server = Server::bind(&socket).unwrap();
        let manager = Arc::new(SessionManager::new(
            Arc::new(Microphone),
            Pipeline {
                asr: Arc::new(NativeModel), vad: None,
                traditional: Some(Arc::new(Traditional::load().unwrap())),
                vocab: Arc::new(Vocab::load_or_empty(&profile.path().join("vocab.toml"))),
                assistant: assistant.clone(), review: None,
            },
        ));
        let task = tokio::spawn(server.run(manager));
        let mut reader = BufReader::new(UnixStream::connect(&socket).await.unwrap());
        reader.get_mut().write_all(b"{\"type\":\"desktop_status\",\"request\":1}\n").await.unwrap();
        assert_eq!(read_event(&mut reader).await["value"]["spelling_status"], "ready");
        let original = "今天新情很好。GitHub 2026";
        if scenario == "exit" {
            tokio::time::sleep(Duration::from_millis(350)).await;
        } else {
            assert_eq!(assistant.process(original, &Default::default(), None), original);
        }
        reader.get_mut().write_all(b"{\"type\":\"desktop_status\",\"request\":2}\n").await.unwrap();
        assert_eq!(read_event(&mut reader).await["value"]["spelling_status"], "unavailable");
        reader.get_mut().write_all(b"{\"type\":\"start\",\"session\":17,\"session_events\":true}\n").await.unwrap();
        assert_eq!(read_event(&mut reader).await["value"], "recording");
        reader.get_mut().write_all(b"{\"type\":\"stop\",\"session\":17}\n").await.unwrap();
        assert_eq!(read_event(&mut reader).await["text"], "請檢查 GitHub。");
        task.abort();
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let profile = tempfile::tempdir().unwrap();
    for (scenario, body) in [
        ("exit", "import time; time.sleep(.25)"),
        ("bad", "import sys; sys.stdin.readline(); print('{bad json}',flush=True)"),
    ] {
        let executable = profile.path().join(format!("csc-ipc-{scenario}"));
        std::fs::write(&executable, format!("#!/usr/bin/python3\nimport json\nprint(json.dumps(dict(v=1,status='ready',provider='CPUExecutionProvider')),flush=True)\n{body}\n")).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = serde_json::json!({
            "executable": executable, "model": profile.path().join("model.onnx"),
            "tokenizer": profile.path().join("tokenizer.json"),
            "model_sha256": "a".repeat(64), "tokenizer_sha256": "b".repeat(64), "threads": 4,
        });
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("desktop_ipc_tests::desktop_status_tracks_owned_csc_failure_while_asr_keeps_responding")
            .env("VOICETYPE_TEST_CSC_IPC_CHILD", scenario)
            .env("VOICETYPE_CSC_PROCESS", config.to_string())
            .env("VOICETYPE_LEARNING_FILE", profile.path().join("learning.json"))
            .status().unwrap();
        assert!(result.success(), "{scenario} CSC failure blocked daemon IPC/ASR");
    }
}

#[tokio::test]
async fn application_receives_capture_failure_and_can_release_without_inserting_text() {
    use voicetype_app_core::{
        Application, DeliveryOutcome, DeliveryPort, ProviderEvent, TargetLease,
    };
    struct MissingMicrophone;
    impl CaptureSource for MissingMicrophone {
        fn begin(&self) -> anyhow::Result<RecordingMark> {
            anyhow::bail!("no fixture microphone")
        }
        fn end(&self, _: RecordingMark) -> Recording {
            panic!("failed capture cannot end")
        }
        fn cancel(&self) {}
        fn suspend(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }
    struct NoInput;
    impl DeliveryPort for NoInput {
        fn commit_if_focused(&mut self, _: &TargetLease, _: &str) -> DeliveryOutcome {
            panic!("failed capture must not insert text")
        }
    }
    let profile = tempfile::tempdir().unwrap();
    let socket = profile.path().join("ipc.sock");
    let server = Server::bind(&socket).unwrap();
    let manager = Arc::new(SessionManager::new(
        Arc::new(MissingMicrophone),
        Pipeline {
            asr: Arc::new(NativeModel),
            vad: None,
            traditional: Some(Arc::new(Traditional::load().unwrap())),
            vocab: Arc::new(Vocab::load_or_empty(&profile.path().join("vocab.toml"))),
            assistant: Arc::new(Assistant::new(Personalization::memory(), None)),
            review: None,
        },
    ));
    let server_task = tokio::spawn(server.run(manager));
    let config = profile.path().join("app");
    tokio::task::spawn_blocking(move || {
        let mut connection = voicetype_app_core::local::LocalConnection::connect_to_process(
            &socket,
            std::process::id(),
            Duration::from_secs(1),
        )
        .unwrap();
        let mut app = Application::open(&config).unwrap();
        app.start_dictation(TargetLease::new("editor", 1), &mut connection)
            .unwrap();
        let (key, event) = connection
            .poll_event(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert!(matches!(event, ProviderEvent::Failed(_)));
        app.provider_event(key, event, &mut NoInput);
        assert!(app.snapshot().dictation.busy);
        let (key, event) = connection
            .poll_event(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        app.provider_event(key, event, &mut NoInput);
        assert!(!app.snapshot().dictation.busy);
        assert!(app.snapshot().dictation.failure.is_some());
        connection.suspend(Duration::from_secs(1)).unwrap();
    })
    .await
    .unwrap();
    server_task.abort();
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
    reader
        .get_mut()
        .write_all(b"{\"type\":\"desktop_suspend\",\"request\":3}\n")
        .await
        .unwrap();
    assert_eq!(
        read_event(&mut reader).await,
        serde_json::json!({"type":"info","value":{"desktop_protocol":1,"request":3,"microphone":"busy"}})
    );
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

#[tokio::test]
async fn microphone_handoff_refuses_a_busy_session_and_acknowledges_actual_close_when_idle() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct WarmMicrophone(Arc<AtomicBool>);
    impl CaptureSource for WarmMicrophone {
        fn begin(&self) -> anyhow::Result<RecordingMark> {
            self.0.store(true, Ordering::SeqCst);
            Microphone.begin()
        }
        fn end(&self, mark: RecordingMark) -> Recording {
            Microphone.end(mark)
        }
        fn cancel(&self) {} // Match the real warm backend: keep the idle stream.
        fn suspend(&self) -> anyhow::Result<()> {
            self.0.store(false, Ordering::SeqCst);
            Ok(())
        }
    }
    let profile = tempfile::tempdir().unwrap();
    let socket = profile.path().join("ipc.sock");
    let server = Server::bind(&socket).unwrap();
    let open = Arc::new(AtomicBool::new(false));
    let manager = Arc::new(SessionManager::new(
        Arc::new(WarmMicrophone(open.clone())),
        Pipeline {
            asr: Arc::new(NativeModel),
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
        .write_all(b"{\"type\":\"desktop_status\",\"request\":9}\n")
        .await
        .unwrap();
    assert_eq!(
        read_event(&mut reader).await,
        serde_json::json!({"type":"info","value":{
            "desktop_protocol":1,"request":9,"capabilities":["session_events","suspend"],"session_busy":false,"spelling_status":"disabled"
        }})
    );
    assert!(
        !open.load(Ordering::SeqCst),
        "capability query must not open the microphone"
    );
    reader
        .get_mut()
        .write_all(b"{\"type\":\"start\",\"session\":22,\"session_events\":true}\n")
        .await
        .unwrap();
    assert_eq!(read_event(&mut reader).await["value"], "recording");
    reader
        .get_mut()
        .write_all(b"{\"type\":\"desktop_suspend\",\"request\":1}\n")
        .await
        .unwrap();
    assert_eq!(
        read_event(&mut reader).await,
        serde_json::json!({"type":"info","value":{"desktop_protocol":1,"request":1,"microphone":"busy"}})
    );
    assert!(open.load(Ordering::SeqCst));
    reader
        .get_mut()
        .write_all(b"{\"type\":\"cancel\",\"session\":22}\n")
        .await
        .unwrap();
    assert_eq!(read_event(&mut reader).await["value"], "idle");
    assert!(
        open.load(Ordering::SeqCst),
        "idle must not pretend the warm stream was closed"
    );
    reader
        .get_mut()
        .write_all(b"{\"type\":\"desktop_suspend\",\"request\":2}\n")
        .await
        .unwrap();
    assert_eq!(
        read_event(&mut reader).await,
        serde_json::json!({"type":"info","value":{"desktop_protocol":1,"request":2,"microphone":"closed"}})
    );
    assert!(!open.load(Ordering::SeqCst));
    task.abort();
}

async fn frontend_round_trip(
    asr: Arc<dyn Transcriber>,
    assistant: Arc<Assistant>,
    correction: Option<&str>,
    ack: Option<&'static str>,
    cancel_after_stop: bool,
) -> (Vec<serde_json::Value>, voicetype_app_core::Application) {
    use serde_json::json;
    use voicetype_app_core::{dispatch::LocalDispatcher, local::LocalConnection, Application};
    let profile = tempfile::tempdir().unwrap();
    let engine_socket = profile.path().join("engine.sock");
    let frontend_socket = profile.path().join("frontend.sock");
    let server = Server::bind(&engine_socket).unwrap();
    let manager = Arc::new(SessionManager::new(
        Arc::new(Microphone),
        Pipeline {
            asr,
            vad: None,
            traditional: Some(Arc::new(Traditional::load().unwrap())),
            vocab: Arc::new(Vocab::load_or_empty(&profile.path().join("vocab.toml"))),
            assistant,
            review: None,
        },
    ));
    let server_task = tokio::spawn(server.run(manager));
    let listener = std::os::unix::net::UnixListener::bind(&frontend_socket).unwrap();
    let config = profile.path().join("app");
    let (finish, finish_rx) = std::sync::mpsc::channel();
    let worker = tokio::task::spawn_blocking(move || {
        let engine = LocalConnection::connect_to_process(
            &engine_socket,
            std::process::id(),
            Duration::from_secs(1),
        )
        .unwrap();
        let (peer, _) = listener.accept().unwrap();
        let mut route = LocalDispatcher::accept(peer, engine, Duration::from_secs(1)).unwrap();
        let mut app = Application::open(&config).unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < until {
            if let Err(error) = route.step(&mut app, Duration::from_millis(10)) {
                assert!(ack.is_none(), "unexpected dispatcher failure: {error}");
                return app;
            }
            if finish_rx.try_recv().is_ok() {
                assert!(!app.snapshot().dictation.busy);
                if ack == Some("committed") {
                    assert!(app.retained_text().is_none());
                }
                assert!(app.snapshot().dictation.failure.is_none());
                route.suspend(Duration::from_secs(1)).unwrap();
                return app;
            }
        }
        panic!("frontend session did not complete");
    });
    // Only the OS input frontend is substituted. Actual dispatcher, application,
    // both transports, daemon, session and complete text pipeline run unchanged.
    let mut input = BufReader::new(UnixStream::connect(&frontend_socket).await.unwrap());
    let hello = read_event(&mut input).await;
    assert_eq!(hello["type"], "desktop_hello");
    input
        .get_mut()
        .write_all(
            format!(
                "{}\n",
                json!({"type":"desktop_hello",
        "session":hello["session"],"value":"voicetype.fcitx.v1"})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut delivered = Vec::new();
    for session in 41..=if correction.is_some() { 42 } else { 41 } {
        let start = json!({"type":"start","session":session,"context_id":"editor-77",
            "program":"editor","is_password":false,"context_text":"GitHub","selected_text":"commit"});
        input
            .get_mut()
            .write_all(format!("{start}\n").as_bytes())
            .await
            .unwrap();
        assert_eq!(
            read_event(&mut input).await,
            json!({"type":"state","session":session,"value":"recording"})
        );
        input
            .get_mut()
            .write_all(format!("{}\n", json!({"type":"stop","session":session})).as_bytes())
            .await
            .unwrap();
        if cancel_after_stop {
            input
                .get_mut()
                .write_all(format!("{}\n", json!({"type":"cancel","session":session})).as_bytes())
                .await
                .unwrap();
        }
        let result = read_event(&mut input).await;
        if cancel_after_stop && result == json!({"type":"state","session":session,"value":"idle"}) {
            break;
        }
        assert_eq!(result["type"], "deliver");
        let Some(code) = ack else {
            delivered.push(result);
            input.get_mut().shutdown().await.unwrap();
            let app = worker.await.unwrap();
            server_task.abort();
            return (delivered, app);
        };
        input
            .get_mut()
            .write_all(
                format!(
                    "{}\n",
                    json!({"type":"delivered","session":session,
            "context_id":"editor-77","code":code})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        assert_eq!(
            read_event(&mut input).await,
            json!({"type":"state","session":session,"value":"idle"})
        );
        if session == 41 {
            if let Some(after) = correction {
                input
                    .get_mut()
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"type":"correction","session":session,
                    "context_id":"editor-77","program":"editor","before":result["text"],
                    "after":after,"confirmed":true})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    read_event(&mut input).await,
                    json!({"type":"state","session":session,"value":"correction_saved"})
                );
            }
        }
        delivered.push(result);
    }
    finish.send(()).unwrap();
    let app = worker.await.unwrap();
    server_task.abort();
    (delivered, app)
}

#[tokio::test]
async fn frontend_shortcut_routes_through_the_app_to_local_engine_and_acknowledged_delivery() {
    let (delivered, _) = frontend_round_trip(
        Arc::new(NativeModel),
        Arc::new(Assistant::new(Personalization::memory(), None)),
        None,
        Some("committed"),
        false,
    )
    .await;
    assert_eq!(
        delivered[0],
        serde_json::json!({"type":"deliver","session":41,
        "context_id":"editor-77","text":"請檢查 GitHub。"})
    );
}

#[tokio::test]
async fn frontend_context_preserves_application_surrounding_selection_and_field_scoped_corrections()
{
    use crate::personalization::ContextSnapshot;
    struct Typos;
    impl Transcriber for Typos {
        fn transcribe(&self, _: Utterance<'_>) -> anyhow::Result<String> {
            Ok("请检查 gthub、cmmit 与 psh。".into())
        }
        fn name(&self) -> &str {
            "native-typo-fixture"
        }
    }
    let mut learned = Personalization::memory();
    for (wrong, right, context_id) in [
        ("gthub", "GitHub", "earlier-field"),
        ("cmmit", "commit", "earlier-field"),
        ("psh", "push", "editor-77"),
    ] {
        learned
            .learn(
                wrong,
                right,
                &ContextSnapshot {
                    program: "editor".into(),
                    context_id: context_id.into(),
                    ..Default::default()
                },
            )
            .unwrap();
    }
    let (delivered, _) = frontend_round_trip(
        Arc::new(Typos),
        Arc::new(Assistant::new(learned, None)),
        None,
        Some("committed"),
        false,
    )
    .await;
    assert_eq!(delivered[0]["text"], "請檢查 GitHub、commit 與 push。");
}

#[tokio::test]
async fn confirmed_frontend_correction_learns_from_the_mapped_engine_session() {
    struct Typo;
    impl Transcriber for Typo {
        fn transcribe(&self, _: Utterance<'_>) -> anyhow::Result<String> {
            Ok("请检查 gthub。".into())
        }
        fn name(&self) -> &str {
            "native-typo-fixture"
        }
    }
    let (delivered, _) = frontend_round_trip(
        Arc::new(Typo),
        Arc::new(Assistant::new(Personalization::memory(), None)),
        Some("請檢查 GitHub。"),
        Some("committed"),
        false,
    )
    .await;
    assert_eq!(delivered[0]["text"], "請檢查 gthub。");
    assert_eq!(delivered[1]["text"], "請檢查 GitHub。");
    assert_eq!(delivered[1]["session"], 42);
}

#[tokio::test]
async fn lost_frontend_ack_retains_uncertain_text_without_releasing_or_retrying_the_session() {
    use voicetype_app_core::DeliveryOutcome;
    let (delivered, app) = frontend_round_trip(
        Arc::new(NativeModel),
        Arc::new(Assistant::new(Personalization::memory(), None)),
        None,
        None,
        false,
    )
    .await;
    assert_eq!(delivered.len(), 1);
    let retained = app
        .retained_text()
        .expect("uncertain text must be recoverable");
    assert_eq!(retained.text, "請檢查 GitHub。");
    assert_eq!(retained.reason, DeliveryOutcome::Unconfirmed);
    assert!(
        app.snapshot().dictation.busy,
        "only owned-child cleanup can authorize release"
    );
    assert!(app.snapshot().dictation.failure.is_some());
}

#[tokio::test]
async fn frontend_stop_followed_immediately_by_cancel_never_attempts_delivery() {
    let (delivered, app) = frontend_round_trip(
        Arc::new(NativeModel),
        Arc::new(Assistant::new(Personalization::memory(), None)),
        None,
        Some("committed"),
        true,
    )
    .await;
    assert!(
        delivered.is_empty(),
        "cancelled dictation must not be offered for delivery"
    );
    assert!(app.retained_text().is_none());
    assert!(!app.snapshot().dictation.busy);
}
