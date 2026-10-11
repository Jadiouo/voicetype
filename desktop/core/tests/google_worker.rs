#![cfg(target_os = "linux")]
use std::{fs, io, sync::mpsc, time::Duration};
use voicetype_app_core::{
    google::GoogleBoundary,
    worker::{DesktopWorker, GoogleRuntimeStatus},
    Provider,
};

#[derive(Default)]
struct ReadyCli {
    closed: bool,
    review_pcm: bool,
    stop_notice: Option<mpsc::Sender<()>>,
}
impl GoogleBoundary for ReadyCli {
    fn start_capture(&mut self) -> io::Result<()> {
        if self.closed {
            Err(io::ErrorKind::BrokenPipe.into())
        } else {
            Ok(())
        }
    }
    fn stop_capture_and_drain(&mut self) -> io::Result<()> {
        if let Some(notice) = &self.stop_notice {
            let _ = notice.send(());
        }
        Ok(())
    }
    fn stop_official_voice(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn wait_official_recorder(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn capture_editor(&mut self) -> io::Result<String> {
        Ok("完整句子".into())
    }
    fn settle(&mut self, _: Duration) -> io::Result<()> {
        Ok(())
    }
    fn reject_known_voice_errors(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn cleanup(&mut self) -> io::Result<()> {
        self.closed = true;
        Ok(())
    }
    fn take_review_pcm(&mut self) -> Option<Vec<u8>> {
        self.review_pcm.then(|| vec![0u8; 16_000 * 2 * 8])
    }
}

#[test]
fn selecting_google_only_saves_preference_and_explicit_activation_owns_input() {
    let root = tempfile::tempdir().unwrap();
    let settings = root.path().join("settings");
    let runtime = root.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let worker = DesktopWorker::spawn(settings.clone()).unwrap();
    let selected = worker.select_provider(Provider::Google).unwrap();
    assert_eq!(selected.google_runtime, GoogleRuntimeStatus::Inactive);
    assert!(!selected.input_requested);
    assert!(!runtime.join("voicetype-app-input/owner").exists());
    let loaded = worker
        .activate_google(runtime.clone(), || Ok(ReadyCli::default()), Ok, false, None)
        .unwrap();
    assert_eq!(loaded.google_runtime, GoogleRuntimeStatus::WaitingForInput);
    assert!(loaded.input_requested);
    assert!(runtime.join("voicetype-app-input/owner").exists());
    assert!(worker.google_endpoint().unwrap().unwrap().exists());
    worker.deactivate_google().unwrap();
    assert!(!runtime.join("voicetype-app-input/owner").exists());
    worker.shutdown().unwrap();
    assert_eq!(
        fs::read_to_string(settings.join("desktop.json"))
            .unwrap()
            .contains("google"),
        true
    );
}

use std::os::unix::fs::PermissionsExt;

#[test]
fn google_uses_original_fcitx_target_and_shared_coordinator_once() {
    use serde_json::{json, Value};
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixStream,
    };
    use voicetype_app_core::DictationPhase;
    let root = tempfile::tempdir().unwrap();
    let runtime = root.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let worker = DesktopWorker::spawn(root.path().join("settings")).unwrap();
    worker.select_provider(Provider::Google).unwrap();
    let opens = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let opened = opens.clone();
    worker
        .activate_google(
            runtime,
            move || {
                opened.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(ReadyCli::default())
            },
            Ok,
            false,
            None,
        )
        .unwrap();
    let peer = UnixStream::connect(worker.google_endpoint().unwrap().unwrap()).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut wire = BufReader::new(peer);
    let read = |wire: &mut BufReader<UnixStream>| -> Value {
        let mut line = String::new();
        wire.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let hello = read(&mut wire);
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"desktop_hello",
        "session":hello["session"],"value":"voicetype.fcitx.google-toggle.v2"})
    )
    .unwrap();
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"start","session":41,
        "context_id":"original-field","is_password":false})
    )
    .unwrap();
    assert_eq!(read(&mut wire)["value"], "recording");
    assert_eq!(
        worker.settings().unwrap().settings.dictation.phase,
        Some(DictationPhase::Recording)
    );
    // The public refresh path must reject an active recording before it opens
    // even a missing CLI image; Stop still reaches the resident worker.
    assert!(worker
        .refresh_google_install(root.path().join("missing-official-cli"))
        .is_err());
    assert!(worker
        .refresh_local(root.path().join("missing-local-socket"))
        .is_err());
    assert!(worker.select_provider(Provider::Local).is_err());
    writeln!(wire.get_mut(), "{}", json!({"type":"stop","session":41})).unwrap();
    let delivery = read(&mut wire);
    assert_eq!(delivery["type"], "deliver");
    assert_eq!(delivery["context_id"], "original-field");
    assert_eq!(delivery["text"], "完整句子");
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"delivered","session":41,
        "context_id":"original-field","code":"committed"})
    )
    .unwrap();
    assert_eq!(read(&mut wire)["value"], "idle");
    assert!(!worker.settings().unwrap().settings.dictation.busy);
    assert!(worker.recovery().unwrap().is_none());
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"start","session":42,
        "context_id":"second-field","is_password":false})
    )
    .unwrap();
    assert_eq!(read(&mut wire)["value"], "recording");
    writeln!(wire.get_mut(), "{}", json!({"type":"stop","session":42})).unwrap();
    let second = read(&mut wire);
    assert_eq!(second["type"], "deliver");
    assert_eq!(second["context_id"], "second-field");
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"delivered","session":42,
        "context_id":"second-field","code":"committed"})
    )
    .unwrap();
    assert_eq!(read(&mut wire)["value"], "idle");
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while opens.load(std::sync::atomic::Ordering::Relaxed) < 3 && std::time::Instant::now() < until
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(opens.load(std::sync::atomic::Ordering::Relaxed), 3);
    worker.deactivate_google().unwrap();
    worker.shutdown().unwrap();
}

#[test]
fn slow_local_status_ping_does_not_hold_google_start_or_stop() {
    use serde_json::{json, Value};
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::{UnixListener, UnixStream},
        sync::Arc,
        thread,
        time::Instant,
    };

    let root = tempfile::tempdir().unwrap();
    let runtime = root.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let worker = Arc::new(DesktopWorker::spawn(root.path().join("settings")).unwrap());
    worker.select_provider(Provider::Google).unwrap();
    let (stopped_tx, stopped_rx) = mpsc::channel();
    worker
        .activate_google(
            runtime,
            move || {
                Ok(ReadyCli {
                    stop_notice: Some(stopped_tx.clone()),
                    ..ReadyCli::default()
                })
            },
            Ok,
            false,
            None,
        )
        .unwrap();

    let peer = UnixStream::connect(worker.google_endpoint().unwrap().unwrap()).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut wire = BufReader::new(peer);
    let mut line = String::new();
    wire.read_line(&mut line).unwrap();
    let hello: Value = serde_json::from_str(&line).unwrap();
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"desktop_hello", "session":hello["session"],
               "value":"voicetype.fcitx.google-toggle.v2"})
    )
    .unwrap();

    let socket = root.path().join("slow-local.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(peer.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        assert_eq!(request, "{\"type\":\"ping\"}\n");
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let _ = peer.write_all(b"{\"type\":\"pong\"}\n");
    });
    let (refresh_done_tx, refresh_done_rx) = mpsc::channel();
    let refreshing = worker.clone();
    let refresh = thread::spawn(move || {
        refresh_done_tx
            .send(refreshing.refresh_local(socket))
            .unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let start = Instant::now();
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"start", "session":1,
               "context_id":"original-field", "is_password":false})
    )
    .unwrap();
    line.clear();
    wire.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["value"],
        "recording"
    );
    assert!(
        start.elapsed() < Duration::from_millis(250),
        "Start waited for local ping"
    );
    writeln!(wire.get_mut(), "{}", json!({"type":"stop", "session":1})).unwrap();
    let stopped = stopped_rx.recv_timeout(Duration::from_millis(250));
    let ping_still_pending = refresh_done_rx.try_recv().is_err();
    release_tx.send(()).unwrap();
    assert!(stopped.is_ok(), "Stop waited for local ping");
    assert!(ping_still_pending, "ping finished before Stop progressed");
    line.clear();
    wire.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["type"],
        "deliver"
    );
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"delivered", "session":1,
               "context_id":"original-field", "code":"committed"})
    )
    .unwrap();
    line.clear();
    wire.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["value"],
        "idle"
    );
    refresh_done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    refresh.join().unwrap();
    server.join().unwrap();
    worker.deactivate_google().unwrap();
    worker.shutdown().unwrap();
}

#[test]
fn rapid_next_start_waits_for_idle_prewarm_without_starting_capture_early() {
    use serde_json::{json, Value};
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixStream,
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc, Arc,
        },
    };
    let root = tempfile::tempdir().unwrap();
    let runtime = root.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let worker = DesktopWorker::spawn(root.path().join("settings")).unwrap();
    worker.select_provider(Provider::Google).unwrap();
    let (warm_started, started) = mpsc::channel();
    let (release_warm, release) = mpsc::channel();
    let opens = Arc::new(AtomicUsize::new(0));
    let count = opens.clone();
    worker
        .activate_google(
            runtime,
            move || {
                if count.fetch_add(1, Ordering::SeqCst) == 1 {
                    warm_started.send(()).unwrap();
                    release.recv().unwrap();
                }
                Ok(ReadyCli::default())
            },
            Ok,
            false,
            None,
        )
        .unwrap();
    let peer = UnixStream::connect(worker.google_endpoint().unwrap().unwrap()).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut wire = BufReader::new(peer);
    let read = |wire: &mut BufReader<UnixStream>| -> Value {
        let mut line = String::new();
        wire.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let hello = read(&mut wire);
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"desktop_hello", "session":hello["session"],
        "value":"voicetype.fcitx.google-toggle.v2"})
    )
    .unwrap();
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"start", "session":1,
        "context_id":"field", "is_password":false})
    )
    .unwrap();
    assert_eq!(read(&mut wire)["value"], "recording");
    writeln!(wire.get_mut(), "{}", json!({"type":"stop", "session":1})).unwrap();
    assert_eq!(read(&mut wire)["type"], "deliver");
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"delivered", "session":1,
        "context_id":"field", "code":"committed"})
    )
    .unwrap();
    assert_eq!(read(&mut wire)["value"], "idle");
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        worker.settings().unwrap().google_runtime,
        GoogleRuntimeStatus::Prewarming
    );
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"start", "session":2,
        "context_id":"field", "is_password":false})
    )
    .unwrap();
    wire.get_mut()
        .set_read_timeout(Some(Duration::from_millis(80)))
        .unwrap();
    let mut early = String::new();
    let error = wire.read_line(&mut early).unwrap_err();
    assert!(matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    ));
    assert!(early.is_empty());
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    release_warm.send(()).unwrap();
    wire.get_mut()
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    assert_eq!(read(&mut wire)["value"], "recording");
    writeln!(wire.get_mut(), "{}", json!({"type":"cancel", "session":2})).unwrap();
    assert_eq!(read(&mut wire)["value"], "idle");
    worker.deactivate_google().unwrap();
    worker.shutdown().unwrap();
}

#[test]
fn review_audio_is_saved_only_after_opt_in_and_confirmed_original_target_delivery() {
    use serde_json::{json, Value};
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixStream,
    };
    use voicetype_app_core::{review::ReviewStore, review_collector::Collector};
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("settings");
    let runtime = root.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let store = ReviewStore::open(config.join("review.json"), config.join("review")).unwrap();
    let revision = store.settings().unwrap().revision;
    store.set_enabled(&revision, true).unwrap();
    let collector =
        Collector::with_paths(config.join("review.json"), config.join("review")).unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while collector.status()["accepting"] != true && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(collector.status()["accepting"], true);
    let worker = DesktopWorker::spawn(config).unwrap();
    worker.select_provider(Provider::Google).unwrap();
    worker
        .activate_google(
            runtime,
            || {
                Ok(ReadyCli {
                    closed: false,
                    review_pcm: true,
                    stop_notice: None,
                })
            },
            |_| Ok("校正後".into()),
            false,
            Some(collector),
        )
        .unwrap();
    let peer = UnixStream::connect(worker.google_endpoint().unwrap().unwrap()).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut wire = BufReader::new(peer);
    let read = |wire: &mut BufReader<UnixStream>| -> Value {
        let mut line = String::new();
        wire.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let hello = read(&mut wire);
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"desktop_hello","session":hello["session"],
        "value":"voicetype.fcitx.google-toggle.v2"})
    )
    .unwrap();
    let mut saved = false;
    for session in 1..=20 {
        writeln!(
            wire.get_mut(),
            "{}",
            json!({"type":"start","session":session,
            "context_id":"field","is_password":false})
        )
        .unwrap();
        assert_eq!(read(&mut wire)["value"], "recording");
        writeln!(
            wire.get_mut(),
            "{}",
            json!({"type":"stop","session":session})
        )
        .unwrap();
        let delivery = read(&mut wire);
        assert_eq!(delivery["text"], "校正後");
        writeln!(
            wire.get_mut(),
            "{}",
            json!({"type":"delivered","session":session,
            "context_id":"field","code":"committed"})
        )
        .unwrap();
        assert_eq!(read(&mut wire)["value"], "idle");
        let until = std::time::Instant::now() + Duration::from_millis(300);
        loop {
            let items = store
                .list(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                )
                .unwrap_or_default();
            if items.len() == 1 {
                assert_eq!(items[0].asr_text, "完整句子");
                assert_eq!(items[0].output_text, "校正後");
                assert_eq!(items[0].duration_ms, 8000);
                saved = true;
                break;
            }
            if std::time::Instant::now() >= until {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if saved {
            break;
        }
    }
    assert!(
        saved,
        "an opted-in, delivered sample must reach the shared store"
    );
    worker.deactivate_google().unwrap();
    worker.shutdown().unwrap();
}

#[test]
fn failed_idle_prewarm_is_visible_and_does_not_start_a_second_microphone() {
    use serde_json::{json, Value};
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixStream,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };
    let root = tempfile::tempdir().unwrap();
    let runtime = root.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let worker = DesktopWorker::spawn(root.path().join("settings")).unwrap();
    worker.select_provider(Provider::Google).unwrap();
    let opens = Arc::new(AtomicUsize::new(0));
    let attempts = opens.clone();
    worker
        .activate_google(
            runtime.clone(),
            move || {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(ReadyCli::default())
                } else {
                    Err(io::ErrorKind::NotFound.into())
                }
            },
            Ok,
            false,
            None,
        )
        .unwrap();
    let peer = UnixStream::connect(worker.google_endpoint().unwrap().unwrap()).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut wire = BufReader::new(peer);
    let read = |wire: &mut BufReader<UnixStream>| -> Value {
        let mut line = String::new();
        wire.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let hello = read(&mut wire);
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"desktop_hello","session":hello["session"],
        "value":"voicetype.fcitx.google-toggle.v2"})
    )
    .unwrap();
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"start","session":1,
        "context_id":"field","is_password":false})
    )
    .unwrap();
    assert_eq!(read(&mut wire)["value"], "recording");
    writeln!(wire.get_mut(), "{}", json!({"type":"stop","session":1})).unwrap();
    assert_eq!(read(&mut wire)["type"], "deliver");
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"delivered","session":1,
        "context_id":"field","code":"committed"})
    )
    .unwrap();
    assert_eq!(read(&mut wire)["value"], "idle");
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while worker.settings().unwrap().google_runtime != GoogleRuntimeStatus::Failed
        && std::time::Instant::now() < until
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        worker.settings().unwrap().google_runtime,
        GoogleRuntimeStatus::Failed
    );
    assert!(runtime.join("voicetype-app-input/owner").exists());
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"start","session":2,
        "context_id":"field","is_password":false})
    )
    .unwrap();
    assert_eq!(read(&mut wire)["type"], "error");
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    worker.deactivate_google().unwrap();
    assert!(!runtime.join("voicetype-app-input/owner").exists());
    worker.shutdown().unwrap();
}

#[test]
fn failed_unload_keeps_input_lease_until_detached_worker_confirms_cleanup() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    struct Sticky {
        release: Arc<AtomicBool>,
    }
    impl GoogleBoundary for Sticky {
        fn start_capture(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn stop_capture_and_drain(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn stop_official_voice(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn wait_official_recorder(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn capture_editor(&mut self) -> io::Result<String> {
            Ok("完整句子".into())
        }
        fn settle(&mut self, _: Duration) -> io::Result<()> {
            Ok(())
        }
        fn reject_known_voice_errors(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn cleanup(&mut self) -> io::Result<()> {
            if self.release.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            }
        }
    }
    let root = tempfile::tempdir().unwrap();
    let runtime = root.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let worker = DesktopWorker::spawn(root.path().join("settings")).unwrap();
    worker.select_provider(Provider::Google).unwrap();
    let release = Arc::new(AtomicBool::new(false));
    let gate = release.clone();
    worker
        .activate_google(
            runtime.clone(),
            move || {
                Ok(Sticky {
                    release: gate.clone(),
                })
            },
            Ok,
            false,
            None,
        )
        .unwrap();
    assert!(worker.deactivate_google().is_err());
    assert!(runtime.join("voicetype-app-input/owner").exists());
    assert_eq!(
        worker.settings().unwrap().google_runtime,
        GoogleRuntimeStatus::Cleaning
    );
    assert!(worker.select_provider(Provider::Local).is_err());
    release.store(true, Ordering::Release);
    let until = std::time::Instant::now() + Duration::from_secs(2);
    let owner = runtime.join("voicetype-app-input/owner");
    loop {
        let status = worker.settings().unwrap().google_runtime;
        if !owner.exists() && status == GoogleRuntimeStatus::Inactive {
            break;
        }
        assert!(
            std::time::Instant::now() < until,
            "cleanup did not reach Inactive"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    worker.select_provider(Provider::Local).unwrap();
    worker.shutdown().unwrap();
}
