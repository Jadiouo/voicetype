use std::{
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};
use voicetype_app_core::setup::{download_https, DownloadSpec};

#[test]
fn cancelling_a_stalled_https_handshake_does_not_wait_for_network_timeout() {
    let root = tempfile::tempdir().unwrap();
    let destination = root.path().join("partial");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted, accept_event) = mpsc::sync_channel(1);
    let (close, finish) = mpsc::sync_channel(1);
    let server = std::thread::spawn(move || {
        let peer = listener.accept().unwrap().0;
        accepted.send(()).unwrap();
        finish.recv_timeout(Duration::from_secs(8)).unwrap();
        drop(peer);
    });
    let spec: DownloadSpec = serde_json::from_value(serde_json::json!({
        "format":"file", "bytes":11,
        "sha256":"c6ce303f2afd029639c46178cc233a099f531a566a8583e12699f259dc666fd5"
    }))
    .unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let token = cancel.clone();
    let (sent, result) = mpsc::sync_channel(1);
    let downloader = std::thread::spawn(move || {
        let result = download_https(
            &format!("https://{address}/model"),
            &spec,
            &destination,
            &token,
            &mut |_, _| {},
        );
        sent.send(result).unwrap();
    });
    accept_event.recv_timeout(Duration::from_secs(5)).unwrap();
    cancel.store(true, Ordering::Release);
    let result = result
        .recv_timeout(Duration::from_secs(2))
        .expect("cancellation waited on the stalled TLS peer");
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
    close.send(()).unwrap();
    server.join().unwrap();
    downloader.join().unwrap();
}

#[test]
fn model_setup_reports_progress_and_reuses_verified_assets_offline() {
    use std::{fs, io, path::Path, sync::atomic::AtomicUsize};
    use voicetype_app_core::{
        assets::AssetStore,
        setup::{ModelBundle, ModelSetup, ModelSource, SetupPhase},
    };
    struct Source(Arc<AtomicUsize>);
    impl ModelSource for Source {
        fn download(
            &self,
            _: &str,
            spec: &DownloadSpec,
            path: &Path,
            _: &AtomicBool,
            progress: &mut dyn FnMut(u64, u64),
        ) -> io::Result<()> {
            self.0.fetch_add(1, Ordering::Relaxed);
            fs::write(path, b"first-model")?;
            progress(spec.bytes, spec.bytes);
            Ok(())
        }
    }
    let root = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let bundle = ModelBundle {
        manifest: serde_json::from_value(serde_json::json!({
            "schema_version":1,"id":"fixture-model","version":"1","platform":"any",
            "source_url":"https://example.invalid/model","license":"MIT",
            "files":[{"path":"model.onnx","bytes":11,"sha256":"c6ce303f2afd029639c46178cc233a099f531a566a8583e12699f259dc666fd5","executable":false}]
        })).unwrap(),
        download: serde_json::from_value(serde_json::json!({"format":"file","bytes":11,"sha256":"c6ce303f2afd029639c46178cc233a099f531a566a8583e12699f259dc666fd5"})).unwrap(),
    };
    let worker = ModelSetup::with_source(
        root.path().join("assets"),
        vec![bundle],
        Arc::new(Source(calls.clone())),
    )
    .unwrap();
    assert_eq!(worker.status().phase, SetupPhase::NotChecked);
    for _ in 0..2 {
        worker.start().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while worker.status().busy {
            assert!(
                std::time::Instant::now() < deadline,
                "setup did not complete"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(worker.status().phase, SetupPhase::Installed);
    }
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "reopening verified assets should work without downloading again"
    );
    let store = AssetStore::open(root.path().join("assets/fixture-model")).unwrap();
    assert_eq!(
        fs::read(store.active().unwrap().unwrap().join("model.onnx")).unwrap(),
        b"first-model"
    );
    worker.shutdown().unwrap();
}

#[test]
fn setup_can_be_cancelled_and_retried_while_settings_remain_responsive() {
    use std::{fs, io, path::Path, sync::atomic::AtomicUsize};
    use voicetype_app_core::{
        setup::{ModelBundle, ModelSetup, ModelSource, SetupPhase},
        worker::DesktopWorker,
        Provider,
    };
    struct Source {
        entered: mpsc::SyncSender<()>,
        calls: AtomicUsize,
    }
    impl ModelSource for Source {
        fn download(
            &self,
            _: &str,
            _: &DownloadSpec,
            path: &Path,
            cancel: &AtomicBool,
            _: &mut dyn FnMut(u64, u64),
        ) -> io::Result<()> {
            if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
                fs::write(path, b"partial")?;
                self.entered.send(()).unwrap();
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while !cancel.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                return Err(io::ErrorKind::Interrupted.into());
            }
            fs::write(path, b"first-model")
        }
    }
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("vocabulary.json"), b"keep vocabulary").unwrap();
    let (entered, signal) = mpsc::sync_channel(1);
    let bundle = ModelBundle {
        manifest: serde_json::from_value(serde_json::json!({
            "schema_version":1,"id":"fixture-model","version":"1","platform":"any",
            "source_url":"https://example.invalid/model","license":"MIT",
            "files":[{"path":"model.onnx","bytes":11,"sha256":"c6ce303f2afd029639c46178cc233a099f531a566a8583e12699f259dc666fd5","executable":false}]
        })).unwrap(),
        download: serde_json::from_value(serde_json::json!({"format":"file","bytes":11,"sha256":"c6ce303f2afd029639c46178cc233a099f531a566a8583e12699f259dc666fd5"})).unwrap(),
    };
    let setup = ModelSetup::with_source(
        root.path().join("assets"),
        vec![bundle],
        Arc::new(Source {
            entered,
            calls: AtomicUsize::new(0),
        }),
    )
    .unwrap();
    let desktop = DesktopWorker::spawn(root.path().join("settings")).unwrap();
    setup.start().unwrap();
    signal.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(setup.status().phase, SetupPhase::Downloading);
    assert_eq!(
        setup.start().err().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(
        desktop
            .select_provider(Provider::Google)
            .unwrap()
            .settings
            .selected_provider,
        Provider::Google
    );
    assert!(setup.cancel().cancel_requested);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while setup.status().busy {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(setup.status().phase, SetupPhase::Cancelled);
    assert!(!fs::read_dir(root.path().join("assets"))
        .unwrap()
        .any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("download-")));
    setup.start().unwrap();
    while setup.status().busy {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(setup.status().phase, SetupPhase::Installed);
    assert_eq!(
        fs::read(root.path().join("vocabulary.json")).unwrap(),
        b"keep vocabulary"
    );
    setup.shutdown().unwrap();
    desktop.shutdown().unwrap();
}
