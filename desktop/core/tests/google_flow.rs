use std::time::Duration;
use voicetype_app_core::google::{GoogleAttempt, GoogleBoundary, GoogleProvider, GoogleText};
use voicetype_app_core::{
    Application, DeliveryOutcome, DeliveryPort, Provider, ProviderCommand, ProviderEvent,
    ProviderPort, TargetLease,
};

#[derive(Default)]
struct ExternalCli {
    steps: Vec<&'static str>,
    snapshots: Vec<&'static str>,
    fail_at: Option<&'static str>,
}

impl ExternalCli {
    fn step(&mut self, name: &'static str) -> std::io::Result<()> {
        self.steps.push(name);
        if self.fail_at == Some(name) {
            Err(std::io::ErrorKind::Other.into())
        } else {
            Ok(())
        }
    }
}

impl GoogleBoundary for ExternalCli {
    fn start_capture(&mut self) -> std::io::Result<()> {
        self.step("start_capture")
    }
    fn stop_capture_and_drain(&mut self) -> std::io::Result<()> {
        self.step("drain")
    }
    fn stop_official_voice(&mut self) -> std::io::Result<()> {
        self.step("stop_official")
    }
    fn wait_official_recorder(&mut self) -> std::io::Result<()> {
        self.step("recorder_exit")
    }
    fn capture_editor(&mut self) -> std::io::Result<String> {
        self.step("capture_editor")?;
        Ok(self.snapshots.remove(0).into())
    }
    fn settle(&mut self, _duration: Duration) -> std::io::Result<()> {
        self.step("settle")
    }
    fn reject_known_voice_errors(&mut self) -> std::io::Result<()> {
        self.step("check_errors")
    }
    fn cleanup(&mut self) -> std::io::Result<()> {
        self.step("cleanup")
    }
}

#[test]
fn stopped_google_voice_drains_before_one_official_stop_and_two_draft_captures() {
    let mut cli = ExternalCli {
        snapshots: vec!["請 push", "請 push 到 GitHub。"],
        ..Default::default()
    };
    let mut attempt = GoogleAttempt::start(&mut cli).unwrap();
    assert_eq!(attempt.finish(&mut cli).unwrap(), "請 push 到 GitHub。");
    assert_eq!(
        cli.steps,
        [
            "start_capture",
            "drain",
            "stop_official",
            "recorder_exit",
            "capture_editor",
            "settle",
            "capture_editor",
            "cleanup",
            "check_errors",
        ]
    );
}

#[test]
fn known_voice_error_rejects_even_a_nonempty_second_draft() {
    let mut cli = ExternalCli {
        snapshots: vec!["部分", "部分文字"],
        fail_at: Some("check_errors"),
        ..Default::default()
    };
    let mut attempt = GoogleAttempt::start(&mut cli).unwrap();
    assert!(attempt.finish(&mut cli).is_err());
    assert_eq!(cli.steps.last(), Some(&"check_errors"));
    assert_eq!(
        cli.steps
            .iter()
            .filter(|step| **step == "stop_official")
            .count(),
        1
    );
}

#[derive(Default)]
struct OriginalField(Vec<String>);
impl DeliveryPort for OriginalField {
    fn commit_if_focused(&mut self, _: &TargetLease, text: &str) -> DeliveryOutcome {
        self.0.push(text.into());
        DeliveryOutcome::Delivered
    }
}

#[test]
fn app_receives_one_google_result_only_after_owned_cleanup() {
    let profile = tempfile::tempdir().unwrap();
    let mut app = Application::open(profile.path()).unwrap();
    app.select_provider(Provider::Google).unwrap();
    let mut provider = GoogleProvider::spawn(ExternalCli {
        snapshots: vec!["早期", "完整句子 GitHub"],
        ..Default::default()
    })
    .unwrap();
    let mut field = OriginalField::default();
    let key = app
        .start_dictation(TargetLease::new("editor", 1), &mut provider)
        .unwrap();
    let (_, event) = provider.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(event, ProviderEvent::Recording));
    app.provider_event(key, event, &mut field);
    app.stop_dictation(&mut provider).unwrap();
    let (_, event) = provider.recv_timeout(Duration::from_secs(1)).unwrap();
    app.provider_event(key, event, &mut field);
    assert_eq!(field.0, ["完整句子 GitHub"]);
    assert!(app.snapshot().dictation.busy);
    let (_, event) = provider.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(event, ProviderEvent::Released));
    app.provider_event(key, event, &mut field);
    assert!(!app.snapshot().dictation.busy);
}

struct SlowDrain {
    cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}
impl GoogleBoundary for SlowDrain {
    fn set_cancellation(&mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.cancellation = Some(flag);
    }
    fn start_capture(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn stop_capture_and_drain(&mut self) -> std::io::Result<()> {
        for _ in 0..100 {
            if self
                .cancellation
                .as_ref()
                .unwrap()
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }
    fn stop_official_voice(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn wait_official_recorder(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn capture_editor(&mut self) -> std::io::Result<String> {
        Ok("too late".into())
    }
    fn settle(&mut self, _: Duration) -> std::io::Result<()> {
        Ok(())
    }
    fn reject_known_voice_errors(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn cleanup(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn cancellation_during_pcm_drain_never_delivers_and_keeps_mic_reserved_until_cleanup() {
    let profile = tempfile::tempdir().unwrap();
    let mut app = Application::open(profile.path()).unwrap();
    app.select_provider(Provider::Google).unwrap();
    let mut provider = GoogleProvider::spawn(SlowDrain { cancellation: None }).unwrap();
    let mut field = OriginalField::default();
    let key = app
        .start_dictation(TargetLease::new("editor", 1), &mut provider)
        .unwrap();
    let (_, recording) = provider.recv_timeout(Duration::from_secs(1)).unwrap();
    app.provider_event(key, recording, &mut field);
    app.stop_dictation(&mut provider).unwrap();
    app.cancel_dictation(&mut provider).unwrap();
    assert!(app.snapshot().dictation.busy);
    let (_, released) = provider.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(released, ProviderEvent::Released));
    app.provider_event(key, released, &mut field);
    assert!(field.0.is_empty());
    assert!(!app.snapshot().dictation.busy);
}

#[test]
fn google_output_uses_the_app_vocabulary_and_traditional_name_policy() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vocab.toml");
    std::fs::write(
        &path,
        "names=['游錫堃']\n[[entry]]\nwrong=['geeho']\nright='GitHub'\n",
    )
    .unwrap();
    let policy = GoogleText::open(&path).unwrap();
    assert_eq!(
        policy
            .apply(
                "游錫堃今天会来，請 push 到 GEEHO。",
                |text, _terms| text.to_owned()
            )
            .unwrap(),
        "游錫堃今天會來，請 push 到 GitHub。"
    );
}

#[test]
fn failed_cleanup_keeps_microphone_reserved_until_retry_confirms_release() {
    struct Flaky {
        attempts: usize,
    }
    impl GoogleBoundary for Flaky {
        fn start_capture(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn stop_capture_and_drain(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn stop_official_voice(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn wait_official_recorder(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn capture_editor(&mut self) -> std::io::Result<String> {
            Ok("候選".into())
        }
        fn settle(&mut self, _: Duration) -> std::io::Result<()> {
            Ok(())
        }
        fn reject_known_voice_errors(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn cleanup(&mut self) -> std::io::Result<()> {
            self.attempts += 1;
            if self.attempts < 3 {
                Err(std::io::ErrorKind::TimedOut.into())
            } else {
                Ok(())
            }
        }
    }
    let profile = tempfile::tempdir().unwrap();
    let mut app = Application::open(profile.path()).unwrap();
    app.select_provider(Provider::Google).unwrap();
    let mut provider = GoogleProvider::spawn(Flaky { attempts: 0 }).unwrap();
    let mut field = OriginalField::default();
    let key = app
        .start_dictation(TargetLease::new("editor", 1), &mut provider)
        .unwrap();
    let (_, recording) = provider.recv_timeout(Duration::from_secs(1)).unwrap();
    app.provider_event(key, recording, &mut field);
    app.stop_dictation(&mut provider).unwrap();
    let (_, failed) = provider.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(failed, ProviderEvent::Failed(_)));
    app.provider_event(key, failed, &mut field);
    assert!(app.snapshot().dictation.busy);
    assert!(provider.recv_timeout(Duration::from_millis(20)).is_none());
    app.cancel_dictation(&mut provider).unwrap();
    let (_, released) = provider.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(released, ProviderEvent::Released));
    app.provider_event(key, released, &mut field);
    assert!(!app.snapshot().dictation.busy);
    assert!(field.0.is_empty());
}

#[test]
fn a_cancel_accepted_after_start_is_never_cleared_by_worker_start() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    struct WaitForCancel {
        cancelled: Option<Arc<AtomicBool>>,
        observed: Arc<AtomicBool>,
        started: Arc<AtomicBool>,
    }
    impl GoogleBoundary for WaitForCancel {
        fn set_cancellation(&mut self, flag: Arc<AtomicBool>) {
            self.cancelled = Some(flag);
        }
        fn start_capture(&mut self) -> std::io::Result<()> {
            self.started.store(true, Ordering::Release);
            let until = std::time::Instant::now() + Duration::from_secs(1);
            while std::time::Instant::now() < until {
                if self.cancelled.as_ref().unwrap().load(Ordering::Acquire) {
                    self.observed.store(true, Ordering::Release);
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(std::io::ErrorKind::TimedOut.into())
        }
        fn stop_capture_and_drain(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn stop_official_voice(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn wait_official_recorder(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn capture_editor(&mut self) -> std::io::Result<String> {
            Ok(String::new())
        }
        fn settle(&mut self, _: Duration) -> std::io::Result<()> {
            Ok(())
        }
        fn reject_known_voice_errors(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn cleanup(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let observed = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicBool::new(false));
    let mut provider = GoogleProvider::spawn(WaitForCancel {
        cancelled: None,
        observed: observed.clone(),
        started: started.clone(),
    })
    .unwrap();
    let profile = tempfile::tempdir().unwrap();
    let mut app = Application::open(profile.path()).unwrap();
    app.select_provider(Provider::Google).unwrap();
    app.start_dictation(TargetLease::new("editor", 1), &mut provider)
        .unwrap();
    app.cancel_dictation(&mut provider).unwrap();
    let _ = provider.recv_timeout(Duration::from_secs(2));
    assert!(
        !started.load(Ordering::Acquire) || observed.load(Ordering::Acquire),
        "accepted cancel was cleared before start_capture"
    );
}

#[test]
fn shutdown_retains_a_failing_boundary_until_cleanup_can_confirm_release() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    struct Failing {
        allow_release: Arc<AtomicBool>,
        cleanup_calls: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
    }
    impl Drop for Failing {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }
    impl GoogleBoundary for Failing {
        fn start_capture(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn stop_capture_and_drain(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn stop_official_voice(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn wait_official_recorder(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn capture_editor(&mut self) -> std::io::Result<String> {
            Ok(String::new())
        }
        fn settle(&mut self, _: Duration) -> std::io::Result<()> {
            Ok(())
        }
        fn reject_known_voice_errors(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn cleanup(&mut self) -> std::io::Result<()> {
            self.cleanup_calls.fetch_add(1, Ordering::AcqRel);
            if self.allow_release.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(std::io::ErrorKind::TimedOut.into())
            }
        }
    }
    let allow_release = Arc::new(AtomicBool::new(false));
    let cleanup_calls = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicBool::new(false));
    let mut provider = GoogleProvider::spawn(Failing {
        allow_release: allow_release.clone(),
        cleanup_calls: cleanup_calls.clone(),
        dropped: dropped.clone(),
    })
    .unwrap();
    let profile = tempfile::tempdir().unwrap();
    let mut app = Application::open(profile.path()).unwrap();
    app.select_provider(Provider::Google).unwrap();
    app.start_dictation(TargetLease::new("editor", 1), &mut provider)
        .unwrap();
    assert!(matches!(
        provider.recv_timeout(Duration::from_secs(1)),
        Some((_, ProviderEvent::Recording))
    ));
    app.cancel_dictation(&mut provider).unwrap();
    assert!(matches!(
        provider.recv_timeout(Duration::from_secs(1)),
        Some((_, ProviderEvent::Failed(_)))
    ));
    let drop_thread = std::thread::spawn(move || drop(provider));
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while cleanup_calls.load(Ordering::Acquire) < 3 && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    let retried = cleanup_calls.load(Ordering::Acquire) >= 3;
    let abandoned = dropped.load(Ordering::Acquire);
    allow_release.store(true, Ordering::Release);
    drop_thread.join().unwrap();
    assert!(retried);
    assert!(!abandoned, "live recorder owner was abandoned");
    assert!(dropped.load(Ordering::Acquire));
}

#[test]
fn dropping_owner_with_full_request_queue_returns_within_shutdown_budget() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    struct StalledStart {
        entered: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
        dropped: Arc<AtomicBool>,
    }
    impl Drop for StalledStart {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }
    impl GoogleBoundary for StalledStart {
        fn start_capture(&mut self) -> std::io::Result<()> {
            self.entered.store(true, Ordering::Release);
            while !self.release.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        }
        fn stop_capture_and_drain(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn stop_official_voice(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn wait_official_recorder(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn capture_editor(&mut self) -> std::io::Result<String> {
            Ok(String::new())
        }
        fn settle(&mut self, _: Duration) -> std::io::Result<()> {
            Ok(())
        }
        fn reject_known_voice_errors(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn cleanup(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let mut provider = GoogleProvider::spawn(StalledStart {
        entered: entered.clone(),
        release: release.clone(),
        dropped: dropped.clone(),
    })
    .unwrap();
    let profile = tempfile::tempdir().unwrap();
    let mut app = Application::open(profile.path()).unwrap();
    app.select_provider(Provider::Google).unwrap();
    let key = app
        .start_dictation(TargetLease::new("editor", 1), &mut provider)
        .unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(1);
    while !entered.load(Ordering::Acquire) {
        assert!(std::time::Instant::now() < until, "start was not entered");
        std::thread::sleep(Duration::from_millis(1));
    }
    for command in [
        ProviderCommand::Stop { key },
        ProviderCommand::Cancel { key },
        ProviderCommand::Stop { key },
        ProviderCommand::Cancel { key },
    ] {
        provider.send(command).unwrap();
    }
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let dropping = std::thread::spawn(move || {
        drop(provider);
        done_tx.send(()).unwrap();
    });
    let bounded = done_rx.recv_timeout(Duration::from_millis(2300)).is_ok();
    assert!(
        !dropped.load(Ordering::Acquire),
        "stalled capture owner was discarded"
    );
    release.store(true, Ordering::Release);
    dropping.join().unwrap();
    assert!(
        bounded,
        "Drop blocked on a full request queue before its deadline"
    );
    let until = std::time::Instant::now() + Duration::from_secs(1);
    while !dropped.load(Ordering::Acquire) {
        assert!(
            std::time::Instant::now() < until,
            "shutdown did not reap owner"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn dropping_owner_with_full_event_queue_keeps_capture_until_verified_cleanup() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    struct RetryCleanup {
        allow_release: Arc<AtomicBool>,
        cleanup_calls: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
    }
    impl Drop for RetryCleanup {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }
    impl GoogleBoundary for RetryCleanup {
        fn start_capture(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn stop_capture_and_drain(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn stop_official_voice(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn wait_official_recorder(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn capture_editor(&mut self) -> std::io::Result<String> {
            Ok(String::new())
        }
        fn settle(&mut self, _: Duration) -> std::io::Result<()> {
            Ok(())
        }
        fn reject_known_voice_errors(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn cleanup(&mut self) -> std::io::Result<()> {
            self.cleanup_calls.fetch_add(1, Ordering::AcqRel);
            if self.allow_release.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(std::io::ErrorKind::TimedOut.into())
            }
        }
    }
    let allow_release = Arc::new(AtomicBool::new(false));
    let cleanup_calls = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicBool::new(false));
    let mut provider = GoogleProvider::spawn(RetryCleanup {
        allow_release: allow_release.clone(),
        cleanup_calls: cleanup_calls.clone(),
        dropped: dropped.clone(),
    })
    .unwrap();
    let profile = tempfile::tempdir().unwrap();
    let mut app = Application::open(profile.path()).unwrap();
    app.select_provider(Provider::Google).unwrap();
    let key = app
        .start_dictation(TargetLease::new("editor", 1), &mut provider)
        .unwrap();
    assert!(matches!(
        provider.recv_timeout(Duration::from_secs(1)),
        Some((_, ProviderEvent::Recording))
    ));
    let until = std::time::Instant::now() + Duration::from_secs(1);
    let mut accepted = 0;
    while accepted < 5 {
        if provider.send(ProviderCommand::Cancel { key }).is_ok() {
            accepted += 1;
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            std::time::Instant::now() < until,
            "could not fill event queue"
        );
    }
    while cleanup_calls.load(Ordering::Acquire) < 5 {
        assert!(
            std::time::Instant::now() < until,
            "worker did not reach full event queue"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let dropping = std::thread::spawn(move || {
        drop(provider);
        done_tx.send(()).unwrap();
    });
    let cleanup_until = std::time::Instant::now() + Duration::from_millis(250);
    while cleanup_calls.load(Ordering::Acquire) < 6 && std::time::Instant::now() < cleanup_until {
        std::thread::sleep(Duration::from_millis(5));
    }
    let cleanup_started_without_receiver = cleanup_calls.load(Ordering::Acquire) >= 6;
    let bounded = done_rx.recv_timeout(Duration::from_millis(2300)).is_ok();
    assert!(bounded, "Drop blocked behind a full event queue");
    dropping.join().unwrap();
    assert!(
        cleanup_started_without_receiver,
        "full event queue prevented shutdown cleanup while provider receiver was alive"
    );
    assert!(
        !dropped.load(Ordering::Acquire),
        "live recorder owner was discarded"
    );
    allow_release.store(true, Ordering::Release);
    let until = std::time::Instant::now() + Duration::from_secs(1);
    while !dropped.load(Ordering::Acquire) {
        assert!(
            std::time::Instant::now() < until,
            "cleanup never reaped boundary"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
