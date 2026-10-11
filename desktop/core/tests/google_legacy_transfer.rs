#![cfg(target_os = "linux")]
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    os::unix::fs::{symlink, PermissionsExt},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use voicetype_app_core::google::legacy_transfer::{
    FcitxIdentity, GoogleShortcutTransfer, LegacyService, TransferPhase, UnitIdentity,
};
use voicetype_app_core::{google::GoogleBoundary, worker::DesktopWorker, Provider, ShortcutChoice};

struct FakeService {
    unit: Mutex<UnitIdentity>,
    fcitx: Mutex<FcitxIdentity>,
    quiet: Mutex<bool>,
    stops: Mutex<u32>,
    starts: Mutex<u32>,
    fail_start_once: Mutex<bool>,
}
impl FakeService {
    fn new() -> Self {
        Self {
            unit: Mutex::new(UnitIdentity {
                fragment: PathBuf::from("/fixture/googlevoiceinput.service"),
                unit_definition_sha256: "a".repeat(64),
                unit_file_state: "enabled".into(),
                enabled: true,
                active: true,
                cgroup: Some(PathBuf::from("/fixture.slice/googlevoiceinput.service")),
            }),
            fcitx: Mutex::new(old_fcitx()),
            quiet: Mutex::new(true),
            stops: Mutex::new(0),
            starts: Mutex::new(0),
            fail_start_once: Mutex::new(false),
        }
    }
}

#[test]
fn pre_restore_fcitx_restart_cannot_count_as_reload_of_original_bytes() {
    let root = tempfile::tempdir().unwrap();
    let (transfer, config, _) = fixture(root.path());
    fs::write(&config, b"Enabled=True\n").unwrap();
    let service = FakeService::new();
    transfer
        .stage_disable(old_fcitx(), service.snapshot().unwrap())
        .unwrap();
    let mut owned = transfer.claim(new_fcitx(), true, &service).unwrap();
    *service.fcitx.lock().unwrap() = restored_fcitx(); // restart while App still owns managed bytes
    transfer.stage_restore(&mut owned, true, &service).unwrap();
    assert!(transfer.finish_restore(restored_fcitx(), &service).is_err());
    let after_restore = FcitxIdentity {
        pid: 14,
        start_ticks: 404,
    };
    *service.fcitx.lock().unwrap() = after_restore.clone();
    transfer.finish_restore(after_restore, &service).unwrap();
}
impl LegacyService for FakeService {
    fn fcitx_identity(&self) -> io::Result<FcitxIdentity> {
        Ok(self.fcitx.lock().unwrap().clone())
    }
    fn snapshot(&self) -> io::Result<UnitIdentity> {
        Ok(self.unit.lock().unwrap().clone())
    }
    fn stop(&self) -> io::Result<()> {
        *self.stops.lock().unwrap() += 1;
        self.unit.lock().unwrap().active = false;
        Ok(())
    }
    fn quiescent(&self, _: Option<&Path>) -> io::Result<bool> {
        Ok(*self.quiet.lock().unwrap())
    }
    fn start(&self) -> io::Result<()> {
        *self.starts.lock().unwrap() += 1;
        let mut fail = self.fail_start_once.lock().unwrap();
        if *fail {
            *fail = false;
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.unit.lock().unwrap().active = true;
        Ok(())
    }
}
struct SharedService(Arc<FakeService>);
impl LegacyService for SharedService {
    fn fcitx_identity(&self) -> io::Result<FcitxIdentity> {
        self.0.fcitx_identity()
    }
    fn snapshot(&self) -> io::Result<UnitIdentity> {
        self.0.snapshot()
    }
    fn stop(&self) -> io::Result<()> {
        self.0.stop()
    }
    fn quiescent(&self, group: Option<&Path>) -> io::Result<bool> {
        self.0.quiescent(group)
    }
    fn start(&self) -> io::Result<()> {
        self.0.start()
    }
}

struct CountedCli(Arc<AtomicUsize>);
impl GoogleBoundary for CountedCli {
    fn start_capture(&mut self) -> io::Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
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
        Ok(String::new())
    }
    fn settle(&mut self, _: Duration) -> io::Result<()> {
        Ok(())
    }
    fn reject_known_voice_errors(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn cleanup(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn read_wire(wire: &mut BufReader<UnixStream>) -> Value {
    let mut line = String::new();
    wire.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}
fn start_wire(wire: &mut BufReader<UnixStream>, session: u64) -> Value {
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"start", "session":session,
        "context_id":"fixture-field", "is_password":false})
    )
    .unwrap();
    read_wire(wire)
}
fn fixture(root: &Path) -> (GoogleShortcutTransfer, PathBuf, PathBuf) {
    let config = root.join("legacy/googlevoiceinput.conf");
    let bridge = root.join("runtime/googlevoiceinput/bridge.lock");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::create_dir_all(bridge.parent().unwrap()).unwrap();
    File::create(&bridge).unwrap();
    fs::set_permissions(&bridge, fs::Permissions::from_mode(0o600)).unwrap();
    let transfer =
        GoogleShortcutTransfer::new(root.join("profile"), config.clone(), bridge.clone()).unwrap();
    (transfer, config, bridge)
}
fn old_fcitx() -> FcitxIdentity {
    FcitxIdentity {
        pid: 11,
        start_ticks: 101,
    }
}
fn new_fcitx() -> FcitxIdentity {
    FcitxIdentity {
        pid: 12,
        start_ticks: 202,
    }
}
fn restored_fcitx() -> FcitxIdentity {
    FcitxIdentity {
        pid: 13,
        start_ticks: 303,
    }
}

#[test]
fn exact_bytes_journal_reload_and_service_lock_gate() {
    let root = tempfile::tempdir().unwrap();
    let (transfer, config, bridge) = fixture(root.path());
    let original = b"# preserved\r\nEnabled = True  \r\nHoldToTalkEnabled=False\r\n[ToggleKey]\r\n0=Control+Caps_Lock\r\n";
    fs::write(&config, original).unwrap();
    let service = FakeService::new();
    transfer
        .stage_disable(old_fcitx(), service.snapshot().unwrap())
        .unwrap();
    let managed = fs::read(&config).unwrap();
    assert_eq!(managed, b"# preserved\r\nEnabled = False  \r\nHoldToTalkEnabled=False\r\n[ToggleKey]\r\n0=Control+Caps_Lock\r\n");
    transfer
        .stage_disable(old_fcitx(), service.snapshot().unwrap())
        .unwrap();
    assert_eq!(
        transfer.phase().unwrap(),
        Some(TransferPhase::DisableStaged)
    );
    assert_eq!(
        transfer
            .claim(old_fcitx(), true, &service)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        transfer
            .claim(new_fcitx(), false, &service)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(*service.stops.lock().unwrap(), 0);

    let outside = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&bridge)
        .unwrap();
    outside.try_lock().unwrap();
    assert_eq!(
        transfer
            .claim(new_fcitx(), true, &service)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    outside.unlock().unwrap();
    let mut owned = transfer.claim(new_fcitx(), true, &service).unwrap();
    assert_eq!(transfer.phase().unwrap(), Some(TransferPhase::AppOwned));
    let stops_before_retry = *service.stops.lock().unwrap();
    assert_eq!(
        transfer
            .claim(new_fcitx(), true, &service)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(*service.stops.lock().unwrap(), stops_before_retry);
    assert!(OpenOptions::new()
        .read(true)
        .write(true)
        .open(&bridge)
        .unwrap()
        .try_lock()
        .is_err());
    fs::write(&config, b"Enabled=False\n# external edit\n").unwrap();
    assert!(transfer.stage_restore(&mut owned, true, &service).is_err());
    assert!(OpenOptions::new()
        .read(true)
        .write(true)
        .open(&bridge)
        .unwrap()
        .try_lock()
        .is_err());
    assert_eq!(
        fs::read(&config).unwrap(),
        b"Enabled=False\n# external edit\n"
    );
    fs::write(&config, &managed).unwrap();
    assert_eq!(
        transfer
            .stage_restore(&mut owned, false, &service)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(OpenOptions::new()
        .read(true)
        .write(true)
        .open(&bridge)
        .unwrap()
        .try_lock()
        .is_err());
    transfer.stage_restore(&mut owned, true, &service).unwrap();
    assert_eq!(fs::read(&config).unwrap(), original);
    assert_eq!(
        transfer.phase().unwrap(),
        Some(TransferPhase::RestoreStaged)
    );
    assert_eq!(*service.starts.lock().unwrap(), 1);
    assert!(transfer.finish_restore(old_fcitx(), &service).is_err());
    assert!(transfer.finish_restore(new_fcitx(), &service).is_err());
    transfer.finish_restore(restored_fcitx(), &service).unwrap();
    assert_eq!(transfer.phase().unwrap(), None);
}

#[test]
fn explicit_crash_restore_reclaims_only_quiet_unowned_bridge() {
    let root = tempfile::tempdir().unwrap();
    let (transfer, config, bridge) = fixture(root.path());
    fs::write(&config, b"Enabled=True\n").unwrap();
    let service = FakeService::new();
    transfer
        .stage_disable(old_fcitx(), service.snapshot().unwrap())
        .unwrap();
    let owned = transfer.claim(new_fcitx(), true, &service).unwrap();
    assert_eq!(
        transfer.reclaim_for_restore(&service).err().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(owned); // simulate a crashed App; the journal still says AppOwned
    *service.quiet.lock().unwrap() = false;
    assert!(transfer.reclaim_for_restore(&service).is_err());
    *service.quiet.lock().unwrap() = true;
    let mut recovered = transfer.reclaim_for_restore(&service).unwrap();
    assert!(OpenOptions::new()
        .read(true)
        .write(true)
        .open(&bridge)
        .unwrap()
        .try_lock()
        .is_err());
    transfer
        .stage_restore(&mut recovered, true, &service)
        .unwrap();
    assert_eq!(fs::read(&config).unwrap(), b"Enabled=True\n");
    transfer.finish_restore(restored_fcitx(), &service).unwrap();
}

#[test]
fn largest_accepted_config_keeps_journal_readable_through_restore() {
    let root = tempfile::tempdir().unwrap();
    let (transfer, config, _) = fixture(root.path());
    let mut original = b"Enabled=True\n#".to_vec();
    original.extend(std::iter::repeat_n(b'x', 16 * 1024 - 1 - original.len()));
    fs::write(&config, &original).unwrap();
    let service = FakeService::new();
    transfer
        .stage_disable(old_fcitx(), service.snapshot().unwrap())
        .unwrap();
    let journal = root.path().join("profile/google-shortcut-rollback.json");
    assert!(fs::metadata(journal).unwrap().len() > 64 * 1024);
    assert_eq!(
        transfer.phase().unwrap(),
        Some(TransferPhase::DisableStaged)
    );
    let mut owned = transfer.claim(new_fcitx(), true, &service).unwrap();
    transfer.stage_restore(&mut owned, true, &service).unwrap();
    assert_eq!(fs::read(config).unwrap(), original);
    transfer.finish_restore(restored_fcitx(), &service).unwrap();
}

#[test]
fn outside_drift_and_symlink_never_get_overwritten() {
    let root = tempfile::tempdir().unwrap();
    let (transfer, config, _) = fixture(root.path());
    let service = FakeService::new();
    fs::write(&config, b"Enabled=True\n[ToggleKey]\n0=Control+Caps_Lock\n").unwrap();
    transfer
        .stage_disable(old_fcitx(), service.snapshot().unwrap())
        .unwrap();
    fs::write(&config, b"Enabled=True\n# user's new edit\n").unwrap();
    assert!(transfer.claim(new_fcitx(), true, &service).is_err());
    assert!(transfer
        .stage_disable(old_fcitx(), service.snapshot().unwrap())
        .is_err());
    assert_eq!(
        fs::read(&config).unwrap(),
        b"Enabled=True\n# user's new edit\n"
    );
    assert_eq!(*service.stops.lock().unwrap(), 0);

    let root = tempfile::tempdir().unwrap();
    let (transfer, config, _) = fixture(root.path());
    let target = root.path().join("elsewhere");
    fs::write(&target, b"Enabled=True\n").unwrap();
    symlink(&target, &config).unwrap();
    assert!(transfer
        .stage_disable(old_fcitx(), service.snapshot().unwrap())
        .is_err());
    assert_eq!(fs::read(target).unwrap(), b"Enabled=True\n");
}

#[test]
fn duplicate_enabled_is_rejected_before_journal_or_write() {
    let root = tempfile::tempdir().unwrap();
    let (transfer, config, _) = fixture(root.path());
    let original = b"Enabled=True\nEnabled=False\n";
    fs::write(&config, original).unwrap();
    assert!(transfer
        .stage_disable(old_fcitx(), FakeService::new().snapshot().unwrap())
        .is_err());
    assert_eq!(fs::read(&config).unwrap(), original);
    assert_eq!(transfer.phase().unwrap(), None);
}

#[test]
fn interrupted_restore_rechecks_bytes_unit_and_quiescence() {
    let root = tempfile::tempdir().unwrap();
    let (transfer, config, _) = fixture(root.path());
    let original = b"Enabled=True\nOther=keep\n";
    fs::write(&config, original).unwrap();
    let service = FakeService::new();
    transfer
        .stage_disable(old_fcitx(), service.snapshot().unwrap())
        .unwrap();
    let ownership = transfer.claim(new_fcitx(), true, &service).unwrap();
    let journal = root.path().join("profile/google-shortcut-rollback.json");
    let mut stored: serde_json::Value =
        serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
    stored["phase"] = "restore_staged".into();
    fs::write(&journal, serde_json::to_vec(&stored).unwrap()).unwrap();
    drop(ownership); // Simulate the App dying between journal and config publication.
    *service.quiet.lock().unwrap() = false;
    assert!(transfer.resume_restore(true, &service).is_err());
    assert_eq!(fs::read(&config).unwrap(), b"Enabled=False\nOther=keep\n");
    *service.quiet.lock().unwrap() = true;
    fs::write(&config, b"Enabled=False\nOther=user-edit\n").unwrap();
    assert!(transfer.resume_restore(true, &service).is_err());
    assert_eq!(*service.starts.lock().unwrap(), 0);
    fs::write(&config, b"Enabled=False\nOther=keep\n").unwrap();
    transfer.resume_restore(true, &service).unwrap();
    assert_eq!(fs::read(&config).unwrap(), original);
    assert_eq!(*service.starts.lock().unwrap(), 1);
    transfer.finish_restore(restored_fcitx(), &service).unwrap();
}

#[test]
fn resident_start_gate_holds_through_handshake_failed_claim_and_restore() {
    let root = tempfile::tempdir().unwrap();
    let (transfer, config, bridge) = fixture(root.path());
    fs::write(&config, b"Enabled=True\n").unwrap();
    let service = Arc::new(FakeService::new());
    let connected_fcitx = FcitxIdentity {
        pid: std::process::id(),
        start_ticks: 202,
    };
    transfer
        .stage_disable(old_fcitx(), service.snapshot().unwrap())
        .unwrap();
    let runtime = root.path().join("app-runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let worker = DesktopWorker::spawn(root.path().join("app-profile")).unwrap();
    worker.select_provider(Provider::Google).unwrap();
    worker.begin_google_transfer_setup().unwrap();
    let captures = Arc::new(AtomicUsize::new(0));
    let counted = captures.clone();
    worker
        .prepare_google_transfer(
            runtime.clone(),
            move || Ok(CountedCli(counted.clone())),
            Ok,
            false,
            None,
        )
        .unwrap();
    // The preclaim route has the one normal InputLease, but no second owner.
    assert!(runtime.join("voicetype-app-input/owner").exists());
    assert!(worker
        .prepare_google_transfer(
            runtime.clone(),
            || Ok(CountedCli(Arc::new(AtomicUsize::new(0)))),
            Ok,
            false,
            None
        )
        .is_err());
    let peer = UnixStream::connect(worker.google_endpoint().unwrap().unwrap()).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut wire = BufReader::new(peer);
    let hello = read_wire(&mut wire);
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"desktop_hello", "session":hello["session"],
        "value":"voicetype.fcitx.google-toggle.v2"})
    )
    .unwrap();
    assert!(worker.google_transfer_route_ready().unwrap());
    assert_eq!(
        worker.google_transfer_route_pid().unwrap(),
        Some(std::process::id())
    );
    assert_eq!(start_wire(&mut wire, 1)["type"], "error");
    assert_eq!(captures.load(Ordering::SeqCst), 0);

    let outside = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&bridge)
        .unwrap();
    outside.try_lock().unwrap();
    assert!(transfer
        .claim(connected_fcitx.clone(), true, service.as_ref())
        .is_err());
    assert_eq!(start_wire(&mut wire, 2)["type"], "error");
    assert_eq!(captures.load(Ordering::SeqCst), 0);
    outside.unlock().unwrap();
    let ownership = transfer
        .claim(connected_fcitx.clone(), true, service.as_ref())
        .unwrap();
    assert_eq!(start_wire(&mut wire, 3)["type"], "error");
    worker
        .complete_google_transfer(ownership, connected_fcitx)
        .unwrap();
    assert_eq!(start_wire(&mut wire, 4)["value"], "recording");
    assert_eq!(captures.load(Ordering::SeqCst), 1);
    writeln!(wire.get_mut(), "{}", json!({"type":"cancel", "session":4})).unwrap();
    assert_eq!(read_wire(&mut wire)["value"], "idle");

    worker.begin_google_transfer_restore().unwrap();
    assert_eq!(start_wire(&mut wire, 5)["type"], "error");
    assert_eq!(captures.load(Ordering::SeqCst), 1);
    worker.deactivate_google().unwrap();
    assert!(OpenOptions::new()
        .read(true)
        .write(true)
        .open(&bridge)
        .unwrap()
        .try_lock()
        .is_err());
    worker
        .restore_google_transfer(transfer, SharedService(service))
        .unwrap();
    assert_eq!(fs::read(&config).unwrap(), b"Enabled=True\n");
    assert!(!runtime.join("voicetype-app-input/owner").exists());
    worker.shutdown().unwrap();
}

#[test]
fn transfer_confirms_alternate_key_before_unblocking_a_google_start() {
    let root = tempfile::tempdir().unwrap();
    let (transfer, config, _) = fixture(root.path());
    fs::write(&config, b"Enabled=True\n").unwrap();
    let service = Arc::new(FakeService::new());
    let fcitx = FcitxIdentity {
        pid: std::process::id(),
        start_ticks: 202,
    };
    transfer
        .stage_disable(old_fcitx(), service.snapshot().unwrap())
        .unwrap();
    let runtime = root.path().join("app-runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let profile = root.path().join("app-profile");
    let worker = DesktopWorker::spawn(profile.clone()).unwrap();
    worker.select_provider(Provider::Google).unwrap();
    worker
        .set_shortcut(Provider::Google, ShortcutChoice::CtrlShiftF9)
        .unwrap();
    worker.begin_google_transfer_setup().unwrap();
    worker
        .prepare_google_transfer(
            runtime,
            || Ok(CountedCli(Arc::new(AtomicUsize::new(0)))),
            Ok,
            false,
            None,
        )
        .unwrap();
    let peer = UnixStream::connect(worker.google_endpoint().unwrap().unwrap()).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut wire = BufReader::new(peer);
    let hello = read_wire(&mut wire);
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"desktop_hello", "session":hello["session"],
        "value":"voicetype.fcitx.google-toggle.ctrl-shift-f9.v3"})
    )
    .unwrap();
    assert!(worker.google_transfer_route_ready().unwrap());
    assert_eq!(start_wire(&mut wire, 1)["type"], "error");
    assert_eq!(
        worker
            .settings()
            .unwrap()
            .settings
            .shortcuts
            .google
            .effective,
        ShortcutChoice::Default
    );
    let ownership = transfer
        .claim(fcitx.clone(), true, service.as_ref())
        .unwrap();
    let ready = worker.complete_google_transfer(ownership, fcitx).unwrap();
    assert_eq!(
        ready.settings.shortcuts.google.effective,
        ShortcutChoice::CtrlShiftF9
    );
    assert_eq!(
        voicetype_app_core::Application::open(&profile)
            .unwrap()
            .snapshot()
            .shortcuts
            .google
            .effective,
        ShortcutChoice::CtrlShiftF9
    );
    assert_eq!(start_wire(&mut wire, 2)["value"], "recording");
    writeln!(wire.get_mut(), "{}", json!({"type":"cancel", "session":2})).unwrap();
    assert_eq!(read_wire(&mut wire)["value"], "idle");
    worker.begin_google_transfer_restore().unwrap();
    worker.deactivate_google().unwrap();
    worker
        .restore_google_transfer(transfer, SharedService(service))
        .unwrap();
    worker.shutdown().unwrap();
}

#[test]
fn failed_shortcut_confirmation_keeps_transfer_owner_and_start_gate_closed() {
    let root = tempfile::tempdir().unwrap();
    let (transfer, config, bridge) = fixture(root.path());
    fs::write(&config, b"Enabled=True\n").unwrap();
    let service = Arc::new(FakeService::new());
    let fcitx = FcitxIdentity {
        pid: std::process::id(),
        start_ticks: 202,
    };
    transfer
        .stage_disable(old_fcitx(), service.snapshot().unwrap())
        .unwrap();
    let runtime = root.path().join("app-runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let profile = root.path().join("app-profile");
    let worker = DesktopWorker::spawn(profile.clone()).unwrap();
    worker.select_provider(Provider::Google).unwrap();
    worker
        .set_shortcut(Provider::Google, ShortcutChoice::CtrlShiftF9)
        .unwrap();
    worker.begin_google_transfer_setup().unwrap();
    worker
        .prepare_google_transfer(
            runtime,
            || Ok(CountedCli(Arc::new(AtomicUsize::new(0)))),
            Ok,
            false,
            None,
        )
        .unwrap();
    let peer = UnixStream::connect(worker.google_endpoint().unwrap().unwrap()).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut wire = BufReader::new(peer);
    let hello = read_wire(&mut wire);
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"desktop_hello", "session":hello["session"],
        "value":"voicetype.fcitx.google-toggle.ctrl-shift-f9.v3"})
    )
    .unwrap();
    assert!(worker.google_transfer_route_ready().unwrap());
    let ownership = transfer
        .claim(fcitx.clone(), true, service.as_ref())
        .unwrap();
    let settings = profile.join("desktop.json");
    let mut edited = fs::read(&settings).unwrap();
    edited.extend_from_slice(b"\n"); // Valid external edit forces a stale-writer conflict.
    fs::write(&settings, &edited).unwrap();
    assert!(worker.complete_google_transfer(ownership, fcitx).is_err());
    assert_eq!(start_wire(&mut wire, 1)["type"], "error");
    assert_eq!(
        voicetype_app_core::Application::open(&profile)
            .unwrap()
            .snapshot()
            .shortcuts
            .google
            .effective,
        ShortcutChoice::Default
    );
    assert!(OpenOptions::new()
        .read(true)
        .write(true)
        .open(&bridge)
        .unwrap()
        .try_lock()
        .is_err());
    worker.begin_google_transfer_restore().unwrap();
    worker.deactivate_google().unwrap();
    worker
        .restore_google_transfer(transfer, SharedService(service))
        .unwrap();
    worker.shutdown().unwrap();
}

#[test]
fn changed_route_identity_parks_lock_and_refuses_start_until_restore() {
    let root = tempfile::tempdir().unwrap();
    let (transfer, config, bridge) = fixture(root.path());
    fs::write(&config, b"Enabled=True\n").unwrap();
    let service = Arc::new(FakeService::new());
    transfer
        .stage_disable(old_fcitx(), service.snapshot().unwrap())
        .unwrap();
    let runtime = root.path().join("app-runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let worker = DesktopWorker::spawn(root.path().join("app-profile")).unwrap();
    worker.select_provider(Provider::Google).unwrap();
    worker.begin_google_transfer_setup().unwrap();
    let captures = Arc::new(AtomicUsize::new(0));
    let counted = captures.clone();
    worker
        .prepare_google_transfer(
            runtime,
            move || Ok(CountedCli(counted.clone())),
            Ok,
            false,
            None,
        )
        .unwrap();
    let peer = UnixStream::connect(worker.google_endpoint().unwrap().unwrap()).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut wire = BufReader::new(peer);
    let hello = read_wire(&mut wire);
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"type":"desktop_hello", "session":hello["session"],
        "value":"voicetype.fcitx.google-toggle.v2"})
    )
    .unwrap();
    assert!(worker.google_transfer_route_ready().unwrap());
    let changed = FcitxIdentity {
        pid: std::process::id() + 1,
        start_ticks: 303,
    };
    let ownership = transfer
        .claim(changed.clone(), true, service.as_ref())
        .unwrap();
    assert!(worker.complete_google_transfer(ownership, changed).is_err());
    assert!(worker.google_transfer_owned().unwrap());
    assert_eq!(start_wire(&mut wire, 1)["type"], "error");
    assert_eq!(captures.load(Ordering::SeqCst), 0);
    assert!(OpenOptions::new()
        .read(true)
        .write(true)
        .open(&bridge)
        .unwrap()
        .try_lock()
        .is_err());
    worker.begin_google_transfer_restore().unwrap();
    worker.deactivate_google().unwrap();
    worker
        .restore_google_transfer(transfer, SharedService(service))
        .unwrap();
    assert_eq!(fs::read(config).unwrap(), b"Enabled=True\n");
    worker.shutdown().unwrap();
}

#[test]
fn failed_legacy_restart_retries_explicit_restore_without_losing_original() {
    let root = tempfile::tempdir().unwrap();
    let (transfer, config, bridge) = fixture(root.path());
    fs::write(&config, b"Enabled=True\r\n").unwrap();
    let service = FakeService::new();
    transfer
        .stage_disable(old_fcitx(), service.snapshot().unwrap())
        .unwrap();
    let mut ownership = transfer.claim(new_fcitx(), true, &service).unwrap();
    *service.fail_start_once.lock().unwrap() = true;
    assert!(transfer
        .stage_restore(&mut ownership, true, &service)
        .is_err());
    assert_eq!(
        transfer.phase().unwrap(),
        Some(TransferPhase::RestoreStaged)
    );
    assert_eq!(fs::read(&config).unwrap(), b"Enabled=True\r\n");
    assert!(OpenOptions::new()
        .read(true)
        .write(true)
        .open(&bridge)
        .unwrap()
        .try_lock()
        .is_ok());
    transfer
        .stage_restore(&mut ownership, true, &service)
        .unwrap();
    assert_eq!(*service.starts.lock().unwrap(), 2);
    transfer.finish_restore(restored_fcitx(), &service).unwrap();
}

#[test]
fn staged_handoff_can_be_aborted_without_stopping_old_service() {
    let root = tempfile::tempdir().unwrap();
    let (transfer, config, _) = fixture(root.path());
    let original = b"Enabled=True\n# keep\n";
    fs::write(&config, original).unwrap();
    let service = FakeService::new();
    transfer
        .stage_disable(old_fcitx(), service.snapshot().unwrap())
        .unwrap();
    transfer.abort_staged(old_fcitx(), true, &service).unwrap();
    assert_eq!(fs::read(&config).unwrap(), original);
    assert_eq!(*service.stops.lock().unwrap(), 0);
    assert_eq!(*service.starts.lock().unwrap(), 0);
    assert_eq!(
        transfer.phase().unwrap(),
        Some(TransferPhase::RestoreStaged)
    );
    assert!(transfer.finish_restore(old_fcitx(), &service).is_err());
    transfer.finish_restore(new_fcitx(), &service).unwrap();
    assert_eq!(transfer.phase().unwrap(), None);
}

#[test]
fn failed_claim_after_stop_can_explicitly_restore_original_service() {
    let root = tempfile::tempdir().unwrap();
    let (transfer, config, bridge) = fixture(root.path());
    fs::write(&config, b"Enabled=True\n").unwrap();
    let service = FakeService::new();
    transfer
        .stage_disable(old_fcitx(), service.snapshot().unwrap())
        .unwrap();
    let outside = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&bridge)
        .unwrap();
    outside.try_lock().unwrap();
    assert_eq!(
        transfer
            .claim(new_fcitx(), true, &service)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(*service.stops.lock().unwrap(), 1);
    assert!(!service.snapshot().unwrap().active);
    assert!(transfer.abort_staged(new_fcitx(), true, &service).is_err());
    outside.unlock().unwrap();
    transfer.abort_staged(new_fcitx(), true, &service).unwrap();
    assert_eq!(fs::read(&config).unwrap(), b"Enabled=True\n");
    assert!(service.snapshot().unwrap().active);
    assert_eq!(*service.starts.lock().unwrap(), 1);
    transfer.finish_restore(restored_fcitx(), &service).unwrap();
}
