#![cfg(target_os = "linux")]

use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
use voicetype_app_core::runtime::{LocalRuntimePaths, OwnedLocal};

fn fixture() -> (tempfile::TempDir, LocalRuntimePaths) {
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("engine");
    // The external process/model boundary is a real child and real Unix socket.
    // It records capture attempts and checks its actual environment.
    fs::write(
        &binary,
        r#"#!/usr/bin/python3
import json, os, socket, time
assert os.environ['VOICETYPE_ASR_PROFILE'] == 'nano'
assert os.environ['CUDA_VISIBLE_DEVICES'] == ''
assert os.path.isdir(os.environ['VOICETYPE_NANO_MODEL_DIR'])
assert os.path.isfile(os.environ['VOICETYPE_NANO_VAD_MODEL'])
assert os.environ['XDG_CONFIG_HOME'].startswith(os.environ['HOME'].rsplit('/', 1)[0])
open(os.path.join(os.environ['HOME'], 'endpoint'), 'w').write(os.environ['VOICETYPE_SOCKET'])
open(os.path.join(os.environ['HOME'], 'pid'), 'w').write(str(os.getpid()))
open(os.path.join(os.environ['HOME'], 'vocab-path'), 'w').write(os.environ.get('VOICETYPE_VOCAB', ''))
open(os.path.join(os.environ['HOME'], 'review-paths'), 'w').write(json.dumps([os.environ.get('VOICETYPE_REVIEW_CONFIG'), os.environ.get('VOICETYPE_REVIEW_ROOT')]))
open(os.path.join(os.environ['HOME'], 'spelling-config'), 'w').write(os.environ.get('VOICETYPE_CSC_PROCESS', ''))
with socket.socket(socket.AF_UNIX) as listener:
    listener.bind(os.environ['VOICETYPE_SOCKET'])
    listener.listen(1)
    peer, _ = listener.accept()
    with peer, peer.makefile('r') as stream:
        for line in stream:
            command = json.loads(line)
            if command['type'] == 'desktop_status':
                response = {'type':'info','value':{
                    'desktop_protocol':1,'request':command['request'],
                    'session_busy':False,'capabilities':['session_events','suspend']}}
            elif command['type'] == 'start':
                open(os.path.join(os.environ['HOME'], 'capture'), 'w').close()
                response = {'type':'state','session':command['session'],'value':'recording'}
            elif command['type'] == 'cancel':
                continue # Deliberately never acknowledges release: must reap.
            elif command['type'] == 'stop':
                peer.sendall((json.dumps({'type':'result','session':command['session'],
                    'text':'請 review GitHub pull request，保留 Antigravity。'})+'\n').encode())
                response = {'type':'state','session':command['session'],'value':'idle'}
            else:
                raise AssertionError('unexpected command')
            peer.sendall((json.dumps(response)+'\n').encode())
# Like the real daemon, stay resident if a controller disappears.
while True: time.sleep(1)
"#,
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let models = root.path().join("models");
    fs::create_dir(&models).unwrap();
    let vad = root.path().join("vad.onnx");
    fs::write(&vad, "native-model-fixture").unwrap();
    let paths = LocalRuntimePaths {
        executable: binary,
        model_dir: models,
        vad_model: vad,
        profile: root.path().join("app-data"),
        vocabulary: root.path().join("vocab.toml"),
        review_config: root.path().join("review.json"),
        review_root: root.path().join("review"),
        spelling: None,
    };
    (root, paths)
}

#[test]
fn owned_engine_receives_only_the_explicit_verified_spelling_bundle() {
    use voicetype_text::spelling::SpellingPaths;
    let (root, mut paths) = fixture();
    let spelling = SpellingPaths {
        executable: root.path().join("spelling/voicetype-csc"),
        model: root.path().join("spelling/model.onnx"),
        tokenizer: root.path().join("spelling/tokenizer.json"),
        model_sha256: "a".repeat(64),
        tokenizer_sha256: "b".repeat(64),
        threads: 4,
    };
    paths.spelling = Some(spelling.clone());
    let mut runtime = OwnedLocal::start(&paths, Duration::from_secs(2)).unwrap();
    assert_eq!(
        fs::read_to_string(paths.profile.join("home/spelling-config")).unwrap(),
        serde_json::to_string(&spelling).unwrap()
    );
    assert!(!paths.profile.join("home/capture").exists());
    runtime.shutdown().unwrap();
    paths.spelling = None;
    let mut runtime = OwnedLocal::start(&paths, Duration::from_secs(2)).unwrap();
    assert_eq!(
        fs::read_to_string(paths.profile.join("home/spelling-config")).unwrap(),
        ""
    );
    runtime.shutdown().unwrap();
}

#[test]
fn reaped_engine_releases_profile_even_if_an_unrelated_fork_inherited_the_lock() {
    let (_root, paths) = fixture();
    let mut runtime = OwnedLocal::start(&paths, Duration::from_secs(2)).unwrap();
    let mut signal = [0; 2];
    // A concurrent fork inherits the open file description carrying flock.
    // It deliberately stays alive through shutdown without executing Rust.
    unsafe {
        assert_eq!(libc::pipe(signal.as_mut_ptr()), 0);
        let child = libc::fork();
        assert!(child >= 0);
        if child == 0 {
            libc::close(signal[1]);
            let mut byte = 0u8;
            libc::read(signal[0], (&mut byte as *mut u8).cast(), 1);
            libc::_exit(0);
        }
        libc::close(signal[0]);
        runtime.shutdown().unwrap();
        // Capture the result before releasing the fork, so failure is evidence
        // of the inherited lock rather than an arbitrary sleep/race.
        let restarted = OwnedLocal::start(&paths, Duration::from_secs(2));
        libc::write(signal[1], b"x".as_ptr().cast(), 1);
        libc::close(signal[1]);
        let mut status = 0;
        assert_eq!(libc::waitpid(child, &mut status, 0), child);
        let mut restarted = restarted.unwrap();
        restarted.shutdown().unwrap();
    }
}

#[test]
fn owned_engine_receives_the_app_vocabulary_without_inheriting_ambient_flags() {
    use voicetype_app_core::vocabulary::Vocabulary;
    let (_root, paths) = fixture();
    let mut editor = Vocabulary::open(paths.vocabulary.clone()).unwrap();
    editor
        .put(
            &editor.snapshot().revision,
            None,
            vec!["geeho".into()],
            "GitHub".into(),
        )
        .unwrap();
    let mut runtime = OwnedLocal::start(&paths, Duration::from_secs(2)).unwrap();
    assert_eq!(
        fs::read_to_string(paths.profile.join("home/vocab-path")).unwrap(),
        paths.vocabulary.to_str().unwrap()
    );
    assert!(!paths.profile.join("home/capture").exists());
    runtime.shutdown().unwrap();
}

#[test]
fn preparing_an_owned_cpu_engine_does_not_record_and_shutdown_reaps_it() {
    let (_root, paths) = fixture();
    let mut runtime = OwnedLocal::start(&paths, Duration::from_secs(2)).unwrap();
    let pid = runtime.process_id().unwrap();
    assert!(!paths.profile.join("home/capture").exists());
    assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
    // A second app owner may not launch another engine in the same profile.
    assert!(OwnedLocal::start(&paths, Duration::from_millis(100)).is_err());
    runtime.shutdown().unwrap();
    assert_eq!(runtime.process_id(), None);
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    let mut restarted = OwnedLocal::start(&paths, Duration::from_secs(2)).unwrap();
    restarted.shutdown().unwrap();
}

#[test]
fn input_handoff_is_explicit_exclusive_and_removed_before_shutdown() {
    use voicetype_app_core::worker::DesktopWorker;
    let (root, paths) = fixture();
    let runtime = root.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let worker = DesktopWorker::spawn(root.path().join("settings")).unwrap();
    assert!(worker.enable_local_input(runtime.clone()).is_err());
    let profile = paths.profile.clone();
    worker.activate_local(paths).unwrap();
    let route = runtime.join("voicetype-app-input/owner");
    assert!(!route.exists());
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(worker.enable_local_input(runtime.clone()).is_err());
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        worker
            .enable_local_input(runtime.clone())
            .unwrap()
            .input_requested
    );
    let record = fs::read_to_string(&route).unwrap();
    assert_eq!(
        record,
        format!(
            "voicetype-input-v1\n{}\n{}\n",
            std::process::id(),
            worker.local_endpoint().unwrap().unwrap().display()
        )
    );
    assert_eq!(
        fs::metadata(&route).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!profile.join("home/capture").exists());
    let (other_root, other_paths) = fixture();
    let other = DesktopWorker::spawn(other_root.path().join("settings")).unwrap();
    other.activate_local(other_paths).unwrap();
    assert!(other.enable_local_input(runtime.clone()).is_err());
    assert_eq!(fs::read_to_string(&route).unwrap(), record);
    // Another process can fork while the lease is active. Its inherited file
    // description must not keep the route locked after this owner withdraws.
    let mut signal = [0; 2];
    unsafe {
        assert_eq!(libc::pipe(signal.as_mut_ptr()), 0);
        let fork = libc::fork();
        assert!(fork >= 0);
        if fork == 0 {
            libc::close(signal[1]);
            let mut byte = 0u8;
            libc::read(signal[0], (&mut byte as *mut u8).cast(), 1);
            libc::_exit(0);
        }
        libc::close(signal[0]);
        worker.deactivate_local().unwrap();
        assert!(!route.exists());
        assert!(!worker.settings().unwrap().input_requested);
        let next_owner = other.enable_local_input(runtime);
        libc::write(signal[1], b"x".as_ptr().cast(), 1);
        libc::close(signal[1]);
        let mut status = 0;
        assert_eq!(libc::waitpid(fork, &mut status, 0), fork);
        assert!(next_owner.unwrap().input_requested);
    }
    other.shutdown().unwrap();
    assert!(!route.exists());
    worker.shutdown().unwrap();
}

#[test]
#[ignore = "requires the native Fcitx harness and packaged module; run explicitly in Linux CI"]
fn installed_fcitx_and_resident_worker_complete_one_mixed_language_dictation() {
    use std::{path::PathBuf, process::Command, thread, time::Instant};
    use voicetype_app_core::{
        assets::AssetManifest,
        input_install::FcitxInstaller,
        worker::{DesktopWorker, LocalRuntimeStatus},
        DictationPhase,
    };
    let (root, paths) = fixture();
    let profile = paths.profile.clone();
    let runtime = root.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let source = PathBuf::from(std::env::var_os("VOICETYPE_TEST_INPUT_BUNDLE").unwrap());
    let manifest: AssetManifest =
        serde_json::from_slice(&fs::read(source.join("manifest.json")).unwrap()).unwrap();
    let installer = FcitxInstaller::new(
        root.path().join("settings"),
        root.path().join("fcitx/addon/voicetype.conf"),
        manifest,
    )
    .unwrap();
    installer.install(&source).unwrap();
    let worker = DesktopWorker::spawn(root.path().join("settings")).unwrap();
    worker.activate_local(paths).unwrap();
    worker.enable_local_input(runtime.clone()).unwrap();
    let mut frontend = Command::new(std::env::var_os("VOICETYPE_TEST_FCITX").unwrap())
        .arg(root.path())
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(7);
    let wait = |condition: &dyn Fn() -> bool| {
        while !condition() {
            assert!(
                Instant::now() < until,
                "native frontend/worker did not complete"
            );
            thread::sleep(Duration::from_millis(10));
        }
    };
    wait(&|| worker.settings().unwrap().local_runtime == LocalRuntimeStatus::Ready);
    assert!(!profile.join("home/capture").exists());
    fs::write(root.path().join("control"), "start").unwrap();
    wait(&|| {
        worker.settings().unwrap().settings.dictation.phase == Some(DictationPhase::Recording)
    });
    assert!(profile.join("home/capture").exists());
    fs::write(root.path().join("control"), "stop").unwrap();
    wait(&|| !worker.settings().unwrap().settings.dictation.busy);
    assert_eq!(
        fs::read_to_string(root.path().join("commits")).unwrap(),
        "請 review GitHub pull request，保留 Antigravity。\n"
    );
    assert!(worker.recovery().unwrap().is_none());
    assert!(worker
        .settings()
        .unwrap()
        .settings
        .dictation
        .failure
        .is_none());
    worker.deactivate_local().unwrap();
    assert!(!runtime.join("voicetype-app-input/owner").exists());
    fs::write(root.path().join("control"), "finish").unwrap();
    assert!(frontend.wait().unwrap().success());
    worker.shutdown().unwrap();
    installer.restore().unwrap();
    assert!(!root.path().join("fcitx/addon/voicetype.conf").exists());
}

#[test]
fn losing_the_input_frontend_reaps_a_non_acknowledging_engine_before_releasing_the_app() {
    use serde_json::{json, Value};
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixStream,
        thread,
    };
    use voicetype_app_core::{Application, Provider};
    let (root, paths) = fixture();
    let config = root.path().join("settings");
    let (peer, frontend) = UnixStream::pair().unwrap();
    let worker = thread::spawn(move || {
        let runtime = OwnedLocal::start(&paths, Duration::from_secs(2)).unwrap();
        let pid = runtime.process_id().unwrap();
        let mut session = runtime
            .attach_frontend(peer, Duration::from_secs(1))
            .unwrap();
        let mut app = Application::open(&config).unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < until {
            if session.step(&mut app, Duration::from_millis(10)).is_err() {
                assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
                assert!(!app.snapshot().dictation.busy);
                assert!(app.snapshot().dictation.failure.is_some());
                app.select_provider(Provider::Google).unwrap();
                return;
            }
        }
        panic!("lost frontend did not clean up provider ownership");
    });
    frontend
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut input = BufReader::new(frontend);
    let mut line = String::new();
    input.read_line(&mut line).unwrap();
    let hello: Value = serde_json::from_str(&line).unwrap();
    writeln!(
        input.get_mut(),
        "{}",
        json!({"type":"desktop_hello","session":hello["session"],
        "value":"voicetype.fcitx.v1"})
    )
    .unwrap();
    writeln!(
        input.get_mut(),
        "{}",
        json!({"type":"start","session":41,
        "context_id":"original-field","is_password":false})
    )
    .unwrap();
    line.clear();
    input.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap(),
        json!({"type":"state","session":41,"value":"recording"})
    );
    drop(input);
    worker.join().unwrap();
}

#[test]
#[ignore = "subprocess fixture invoked by the owner-exit test"]
fn runtime_parent_helper() {
    let encoded = std::env::var("VOICETYPE_TEST_PARENT_PATHS").expect("test parent paths");
    let paths: Vec<std::path::PathBuf> = serde_json::from_str(&encoded).unwrap();
    let runtime = OwnedLocal::start(
        &LocalRuntimePaths {
            executable: paths[0].clone(),
            model_dir: paths[1].clone(),
            vad_model: paths[2].clone(),
            profile: paths[3].clone(),
            vocabulary: paths[3].join("vocab.toml"),
            review_config: paths[3].join("review.json"),
            review_root: paths[3].join("review"),
            spelling: None,
        },
        Duration::from_secs(2),
    )
    .unwrap();
    fs::write(
        paths[3].join("child.pid"),
        runtime.process_id().unwrap().to_string(),
    )
    .unwrap();
    // Simulate application exit without Rust destructors.
    std::process::exit(0);
}

#[test]
fn an_idle_engine_crash_is_detected_without_a_frontend_command() {
    use serde_json::{json, Value};
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixStream,
        thread,
    };
    use voicetype_app_core::Application;
    let (root, paths) = fixture();
    let runtime = OwnedLocal::start(&paths, Duration::from_secs(2)).unwrap();
    let pid = runtime.process_id().unwrap();
    let (peer, frontend) = UnixStream::pair().unwrap();
    let frontend = thread::spawn(move || {
        let mut input = BufReader::new(frontend);
        let mut line = String::new();
        input.read_line(&mut line).unwrap();
        let hello: Value = serde_json::from_str(&line).unwrap();
        writeln!(
            input.get_mut(),
            "{}",
            json!({"type":"desktop_hello",
            "session":hello["session"],"value":"voicetype.fcitx.v1"})
        )
        .unwrap();
        input
    });
    let mut session = runtime
        .attach_frontend(peer, Duration::from_secs(1))
        .unwrap();
    let _frontend = frontend.join().unwrap(); // Keep the healthy input connection open.
    let mut app = Application::open(&root.path().join("settings")).unwrap();
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) }, 0);
    let until = std::time::Instant::now() + Duration::from_secs(1);
    let mut detected = false;
    while std::time::Instant::now() < until {
        if session.step(&mut app, Duration::from_millis(10)).is_err() {
            detected = true;
            break;
        }
    }
    let reaped = session.process_id().is_none();
    session.shutdown(&mut app).unwrap();
    assert!(detected, "a dead idle engine was still reported usable");
    assert!(
        reaped,
        "crashed engine was not reaped before failure returned"
    );
    assert!(!app.snapshot().dictation.busy);
}

#[test]
fn desktop_worker_owns_runtime_on_its_resident_thread_and_observes_idle_exit() {
    use serde_json::{json, Value};
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixStream,
        time::Instant,
    };
    use voicetype_app_core::worker::{DesktopWorker, LocalRuntimeStatus};
    let (root, paths) = fixture();
    let capture = paths.profile.join("home/capture");
    let worker = DesktopWorker::spawn(root.path().join("settings")).unwrap();
    assert_eq!(
        worker.settings().unwrap().local_runtime,
        LocalRuntimeStatus::Inactive
    );
    assert!(!capture.exists());
    let view = worker.activate_local(paths).unwrap();
    assert_eq!(view.local_runtime, LocalRuntimeStatus::WaitingForInput);
    assert_eq!(
        serde_json::to_value(&view).unwrap()["spelling_runtime"],
        "unavailable"
    );
    assert!(
        !capture.exists(),
        "activation opened capture without native input"
    );
    let peer = UnixStream::connect(worker.local_endpoint().unwrap().unwrap()).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut input = BufReader::new(peer);
    let mut line = String::new();
    input.read_line(&mut line).unwrap();
    let hello: Value = serde_json::from_str(&line).unwrap();
    writeln!(
        input.get_mut(),
        "{}",
        json!({"type":"desktop_hello",
        "session":hello["session"],"value":"voicetype.fcitx.v1"})
    )
    .unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while worker.settings().unwrap().local_runtime != LocalRuntimeStatus::Ready {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(10));
    }
    // The external child records its actual OS PID. Request completion must not
    // terminate the spawning thread (and consequently its owned child).
    let pid: libc::pid_t = fs::read_to_string(root.path().join("app-data/home/pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    let until = Instant::now() + Duration::from_secs(2);
    while worker.settings().unwrap().local_runtime != LocalRuntimeStatus::Failed {
        assert!(
            Instant::now() < until,
            "worker never observed idle child exit"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    assert!(!capture.exists());
    assert!(worker.local_endpoint().unwrap().is_none());
    assert!(worker.set_spelling_enabled(false).is_err());
    assert_eq!(
        worker.deactivate_local().unwrap().local_runtime,
        LocalRuntimeStatus::Inactive
    );
    assert!(
        !worker
            .set_spelling_enabled(false)
            .unwrap()
            .settings
            .spelling_enabled
    );
    worker.shutdown().unwrap();
}

#[test]
fn desktop_worker_retains_focus_rejected_text_for_explicit_recovery_only() {
    use serde_json::{json, Value};
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixStream,
        time::Instant,
    };
    use voicetype_app_core::{worker::DesktopWorker, DeliveryOutcome, Provider};
    let (root, paths) = fixture();
    let worker = DesktopWorker::spawn(root.path().join("settings")).unwrap();
    worker.activate_local(paths).unwrap();
    let peer = UnixStream::connect(worker.local_endpoint().unwrap().unwrap()).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut input = BufReader::new(peer);
    fn read(input: &mut BufReader<UnixStream>) -> Value {
        let mut line = String::new();
        input.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
    let hello = read(&mut input);
    writeln!(
        input.get_mut(),
        "{}",
        json!({"type":"desktop_hello",
        "session":hello["session"],"value":"voicetype.fcitx.v1"})
    )
    .unwrap();
    writeln!(
        input.get_mut(),
        "{}",
        json!({"type":"start","session":72,
        "context_id":"original-field","is_password":false})
    )
    .unwrap();
    assert_eq!(read(&mut input)["value"], "recording");
    assert!(worker.select_provider(Provider::Google).is_err());
    writeln!(input.get_mut(), "{}", json!({"type":"stop","session":72})).unwrap();
    let delivery = read(&mut input);
    assert_eq!(delivery["type"], "deliver");
    assert_eq!(delivery["context_id"], "original-field");
    writeln!(
        input.get_mut(),
        "{}",
        json!({"type":"delivered","session":72,
        "context_id":"original-field","code":"focus_changed"})
    )
    .unwrap();
    assert_eq!(read(&mut input)["value"], "idle");
    let retained = worker
        .recovery()
        .unwrap()
        .expect("text available to recovery UI");
    assert_eq!(
        retained.text,
        "請 review GitHub pull request，保留 Antigravity。"
    );
    assert_eq!(retained.reason, DeliveryOutcome::FocusChanged);
    assert!(!serde_json::to_string(&worker.settings().unwrap())
        .unwrap()
        .contains("Antigravity"));
    assert!(worker.reload().err().unwrap().contains("卸載"));
    assert_eq!(
        worker.settings().unwrap().local_runtime,
        voicetype_app_core::worker::LocalRuntimeStatus::Ready
    );
    worker.deactivate_local().unwrap();
    worker.reload().unwrap();
    assert_eq!(worker.recovery().unwrap().unwrap().text, retained.text);
    // A stale UI dismissal cannot erase a newer retained result.
    assert!(!worker
        .dismiss_recovery(Provider::Local, "0".into())
        .unwrap());
    assert!(worker.recovery().unwrap().is_some());
    assert!(worker
        .dismiss_recovery(Provider::Local, retained.session)
        .unwrap());
    assert!(worker.recovery().unwrap().is_none());
    worker.select_provider(Provider::Google).unwrap();
    assert!(worker.local_endpoint().unwrap().is_none());
    let until = Instant::now() + Duration::from_secs(1);
    let mut remainder = String::new();
    // Closing ownership ends the socket; no recovery action resubmitted text.
    while input.read_line(&mut remainder).unwrap() != 0 {
        assert!(Instant::now() < until);
    }
    assert!(remainder.is_empty());
    worker.shutdown().unwrap();
}

#[test]
fn public_reload_keeps_effective_spelling_until_engine_is_unloaded() {
    use voicetype_app_core::worker::{DesktopWorker, LocalRuntimeStatus};
    let (root, paths) = fixture();
    let settings = root.path().join("settings");
    let worker = DesktopWorker::spawn(settings.clone()).unwrap();
    assert!(worker.settings().unwrap().settings.spelling_enabled);
    worker.activate_local(paths).unwrap();
    fs::create_dir_all(&settings).unwrap();
    fs::write(
        settings.join("desktop.json"),
        r#"{"schema_version":1,"selected_provider":"local","spelling_enabled":false}"#,
    )
    .unwrap();
    assert!(worker.reload().err().unwrap().contains("卸載"));
    let still_running = worker.settings().unwrap();
    assert!(still_running.settings.spelling_enabled);
    assert_eq!(
        still_running.local_runtime,
        LocalRuntimeStatus::WaitingForInput
    );
    assert!(worker.local_endpoint().unwrap().is_some());
    worker.deactivate_local().unwrap();
    let reloaded = worker.reload().unwrap();
    assert!(!reloaded.settings.spelling_enabled);
    assert_eq!(reloaded.local_runtime, LocalRuntimeStatus::Inactive);
    worker.shutdown().unwrap();
}

#[test]
fn loaded_app_refreshes_owned_spelling_health_without_losing_asr() {
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Instant;
    use voicetype_app_core::worker::{DesktopWorker, LocalRuntimeStatus, SpellingRuntimeStatus};
    let (root, mut paths) = fixture();
    fs::write(
        &paths.executable,
        r#"#!/usr/bin/python3
import json, os, socket, threading, time
marker=os.path.join(os.environ['HOME'],'csc-dead')
slow=os.path.join(os.environ['HOME'],'slow-status')
entered=os.path.join(os.environ['HOME'],'status-entered')
bad=os.path.join(os.environ['HOME'],'bad-status')
bad_count=os.path.join(os.environ['HOME'],'bad-status-count')
def serve(peer):
    with peer, peer.makefile('r') as stream:
        for line in stream:
            command=json.loads(line)
            if command['type']=='desktop_status':
                if os.path.exists(slow):
                    open(entered,'w').close()
                    time.sleep(.2)
                value={'desktop_protocol':1,'request':command['request'],
                    'session_busy':False,'capabilities':['session_events','suspend']}
                if os.path.exists(bad):
                    n=int(open(bad_count).read()) if os.path.exists(bad_count) else 0
                    open(bad_count,'w').write(str(n+1))
                    if open(bad).read().strip()=='unknown':
                        value['spelling_status']='unexpected'
                else:
                    value['spelling_status']='unavailable' if os.path.exists(marker) else 'ready'
                reply={'type':'info','value':value}
            elif command['type']=='start':
                reply={'type':'state','session':command['session'],'value':'recording'}
            elif command['type']=='stop':
                peer.sendall((json.dumps({'type':'result','session':command['session'],
                    'text':'請檢查 GitHub。'})+'\n').encode())
                reply={'type':'state','session':command['session'],'value':'idle'}
            else:
                continue
            peer.sendall((json.dumps(reply)+'\n').encode())
with socket.socket(socket.AF_UNIX) as listener:
    listener.bind(os.environ['VOICETYPE_SOCKET'])
    listener.listen(8)
    while True:
        peer,_=listener.accept()
        threading.Thread(target=serve,args=(peer,),daemon=True).start()
"#,
    )
    .unwrap();
    paths.spelling = Some(voicetype_text::spelling::SpellingPaths {
        executable: root.path().join("csc"),
        model: root.path().join("model.onnx"),
        tokenizer: root.path().join("tokenizer.json"),
        model_sha256: "a".repeat(64),
        tokenizer_sha256: "b".repeat(64),
        threads: 4,
    });
    let worker = DesktopWorker::spawn(root.path().join("settings")).unwrap();
    let loaded = worker.activate_local(paths).unwrap();
    assert_eq!(loaded.spelling_runtime, SpellingRuntimeStatus::Ready);
    let home = root.path().join("app-data/home");
    fs::write(home.join("slow-status"), b"slow").unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while !home.join("status-entered").exists() {
        assert!(Instant::now() < until, "no idle health query");
        std::thread::sleep(Duration::from_millis(1));
    }
    let before = Instant::now();
    assert_eq!(
        worker.settings().unwrap().spelling_runtime,
        SpellingRuntimeStatus::Ready
    );
    assert!(
        before.elapsed() < Duration::from_millis(35),
        "idle health query blocked the command worker"
    );
    fs::remove_file(home.join("slow-status")).unwrap();
    fs::write(home.join("bad-status"), b"missing").unwrap();
    let await_bad_queries = |target: u64| {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            let count = fs::read_to_string(home.join("bad-status-count"))
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            if count >= target {
                break;
            }
            assert!(
                Instant::now() < until,
                "invalid status stopped health refresh"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    await_bad_queries(2);
    fs::write(home.join("bad-status"), b"unknown").unwrap();
    await_bad_queries(4);
    assert_eq!(
        worker.settings().unwrap().spelling_runtime,
        SpellingRuntimeStatus::Ready
    );
    fs::remove_file(home.join("bad-status")).unwrap();
    fs::write(root.path().join("app-data/home/csc-dead"), b"dead").unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    while worker.settings().unwrap().spelling_runtime != SpellingRuntimeStatus::Unavailable {
        assert!(Instant::now() < until, "App kept stale CSC ready status");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        worker.settings().unwrap().local_runtime,
        LocalRuntimeStatus::WaitingForInput
    );
    let peer = UnixStream::connect(worker.local_endpoint().unwrap().unwrap()).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut input = BufReader::new(peer);
    let mut line = String::new();
    input.read_line(&mut line).unwrap();
    let hello: Value = serde_json::from_str(&line).unwrap();
    writeln!(
        input.get_mut(),
        "{}",
        json!({"type":"desktop_hello","session":hello["session"],"value":"voicetype.fcitx.v1"})
    )
    .unwrap();
    writeln!(
        input.get_mut(),
        "{}",
        json!({"type":"start","session":1,"context_id":"editor","is_password":false})
    )
    .unwrap();
    line.clear();
    input.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["value"],
        "recording"
    );
    writeln!(input.get_mut(), "{}", json!({"type":"stop","session":1})).unwrap();
    line.clear();
    input.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["type"],
        "deliver"
    );
    writeln!(
        input.get_mut(),
        "{}",
        json!({"type":"delivered","session":1,"context_id":"editor","code":"committed"})
    )
    .unwrap();
    line.clear();
    input.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["value"],
        "idle"
    );
    worker.deactivate_local().unwrap();
    worker.shutdown().unwrap();
}

#[test]
#[ignore = "exercises the real 30-second optional CSC startup boundary"]
fn near_inner_csc_timeout_still_allows_outer_asr_readiness() {
    use std::time::Instant;
    let (root, mut paths) = fixture();
    let csc = root.path().join("csc-slow-fail");
    fs::write(
        &csc,
        "#!/usr/bin/python3\nimport time,sys\ntime.sleep(30.2)\nsys.exit(1)\n",
    )
    .unwrap();
    fs::set_permissions(&csc, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        &paths.executable,
        r#"#!/usr/bin/python3
import json, os, socket, subprocess, time
config=json.loads(os.environ['VOICETYPE_CSC_PROCESS'])
assert subprocess.call([config['executable']]) != 0
with socket.socket(socket.AF_UNIX) as listener:
    listener.bind(os.environ['VOICETYPE_SOCKET'])
    listener.listen(1)
    peer,_=listener.accept()
    with peer, peer.makefile('r') as stream:
        for line in stream:
            command=json.loads(line)
            if command['type']=='desktop_status':
                peer.sendall((json.dumps({'type':'info','value':{
                    'desktop_protocol':1,'request':command['request'],
                    'capabilities':['session_events','suspend'],'session_busy':False,
                    'spelling_status':'unavailable'}})+'\n').encode())
while True: time.sleep(1)
"#,
    )
    .unwrap();
    paths.spelling = Some(voicetype_text::spelling::SpellingPaths {
        executable: csc,
        model: root.path().join("model.onnx"),
        tokenizer: root.path().join("tokenizer.json"),
        model_sha256: "a".repeat(64),
        tokenizer_sha256: "b".repeat(64),
        threads: 4,
    });
    let started = Instant::now();
    let mut runtime = OwnedLocal::start(&paths, Duration::from_secs(35)).unwrap();
    assert!(started.elapsed() >= Duration::from_secs(30));
    assert!(!runtime.spelling_ready());
    assert!(
        runtime.is_running().unwrap(),
        "ASR must survive CSC startup failure"
    );
    runtime.shutdown().unwrap();
}

#[test]
fn cancelled_engine_that_never_releases_is_reaped_at_the_deadline() {
    use serde_json::{json, Value};
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixStream,
        sync::{Arc, Mutex},
        thread,
        time::Instant,
    };
    use voicetype_app_core::{Application, DictationPhase, SessionClock, SessionFailure};
    struct Clock(Mutex<Instant>);
    impl SessionClock for Clock {
        fn now(&self) -> Instant {
            *self.0.lock().unwrap()
        }
    }
    let (root, paths) = fixture();
    let runtime = OwnedLocal::start(&paths, Duration::from_secs(2)).unwrap();
    let pid = runtime.process_id().unwrap();
    let (peer, frontend) = UnixStream::pair().unwrap();
    let frontend = thread::spawn(move || {
        frontend
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut input = BufReader::new(frontend);
        let mut line = String::new();
        input.read_line(&mut line).unwrap();
        let hello: Value = serde_json::from_str(&line).unwrap();
        writeln!(
            input.get_mut(),
            "{}",
            json!({"type":"desktop_hello",
            "session":hello["session"],"value":"voicetype.fcitx.v1"})
        )
        .unwrap();
        writeln!(
            input.get_mut(),
            "{}",
            json!({"type":"start","session":1,
            "context_id":"original","is_password":false})
        )
        .unwrap();
        line.clear();
        input.read_line(&mut line).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap()["value"],
            "recording"
        );
        writeln!(input.get_mut(), "{}", json!({"type":"cancel","session":1})).unwrap();
        input
    });
    let mut session = runtime
        .attach_frontend(peer, Duration::from_secs(1))
        .unwrap();
    let clock = Arc::new(Clock(Mutex::new(Instant::now())));
    let mut app =
        Application::open_with_clock(&root.path().join("settings"), clock.clone()).unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while app.snapshot().dictation.phase != Some(DictationPhase::Releasing) {
        assert!(Instant::now() < until);
        session.step(&mut app, Duration::from_millis(10)).unwrap();
    }
    let _input = frontend.join().unwrap();
    assert!(app.snapshot().dictation.busy);
    *clock.0.lock().unwrap() += Duration::from_secs(4);
    let failed = session.step(&mut app, Duration::from_millis(10)).is_err();
    let reaped = session.process_id().is_none();
    let released = !app.snapshot().dictation.busy;
    let failure = app.snapshot().dictation.failure;
    session.shutdown(&mut app).unwrap();
    assert!(
        failed,
        "cancel with no acknowledgement stayed busy indefinitely"
    );
    assert!(reaped && released);
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    assert_eq!(failure, Some(SessionFailure::TimedOut));
    assert!(app.retained_text().is_none());
}

#[test]
fn the_ui_can_cancel_a_recording_and_a_hung_engine_is_reaped_without_more_input() {
    use serde_json::{json, Value};
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixStream,
        time::Instant,
    };
    use voicetype_app_core::{worker::DesktopWorker, SessionFailure};
    let (root, paths) = fixture();
    let worker = DesktopWorker::spawn(root.path().join("settings")).unwrap();
    worker.activate_local(paths).unwrap();
    let peer = UnixStream::connect(worker.local_endpoint().unwrap().unwrap()).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut input = BufReader::new(peer);
    let mut line = String::new();
    input.read_line(&mut line).unwrap();
    let hello: Value = serde_json::from_str(&line).unwrap();
    writeln!(
        input.get_mut(),
        "{}",
        json!({"type":"desktop_hello",
        "session":hello["session"],"value":"voicetype.fcitx.v1"})
    )
    .unwrap();
    writeln!(
        input.get_mut(),
        "{}",
        json!({"type":"start","session":33,
        "context_id":"original","is_password":false})
    )
    .unwrap();
    line.clear();
    input.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["value"],
        "recording"
    );
    let pid = fs::read_to_string(root.path().join("app-data/home/pid")).unwrap();
    assert!(worker.cancel_dictation().unwrap().settings.dictation.busy);
    let until = Instant::now() + Duration::from_secs(5);
    while worker.settings().unwrap().settings.dictation.busy {
        assert!(Instant::now() < until, "cancel never completed");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        worker.settings().unwrap().settings.dictation.failure,
        Some(SessionFailure::TimedOut)
    );
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    assert!(worker.recovery().unwrap().is_none());
    assert!(worker.local_endpoint().unwrap().is_none());
    worker.shutdown().unwrap();
}

#[test]
fn application_exit_without_destructors_terminates_the_owned_engine() {
    let (_root, paths) = fixture();
    // Reap this test's orphan ourselves rather than leaving cleanup to PID 1.
    // Other tests retain and wait on their own direct Child handles as usual.
    let mut previous: libc::c_int = 0;
    unsafe {
        assert_eq!(libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut previous), 0);
        assert_eq!(libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1), 0);
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "runtime_parent_helper"])
        .env(
            "VOICETYPE_TEST_PARENT_PATHS",
            serde_json::to_string(&[
                &paths.executable,
                &paths.model_dir,
                &paths.vad_model,
                &paths.profile,
            ])
            .unwrap(),
        )
        .status()
        .unwrap();
    assert!(status.success());
    let pid: libc::pid_t = fs::read_to_string(paths.profile.join("child.pid"))
        .unwrap()
        .parse()
        .unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(1);
    let mut exited = false;
    let mut child_status = 0;
    while std::time::Instant::now() < until {
        let waited = unsafe { libc::waitpid(pid, &mut child_status, libc::WNOHANG) };
        if waited == pid {
            exited = true;
            break;
        }
        assert_eq!(waited, 0);
        std::thread::sleep(Duration::from_millis(10));
    }
    if !exited {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, &mut child_status, 0);
        }
    }
    unsafe {
        assert_eq!(libc::prctl(libc::PR_SET_CHILD_SUBREAPER, previous), 0);
    }
    // process::exit intentionally bypassed TempDir's destructor in the helper.
    let endpoint =
        std::path::PathBuf::from(fs::read_to_string(paths.profile.join("home/endpoint")).unwrap());
    let directory = endpoint.parent().unwrap();
    assert_eq!(directory.parent(), Some(std::env::temp_dir().as_path()));
    assert!(directory
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("voicetype-app-"));
    fs::remove_dir_all(directory).unwrap();
    assert!(exited, "engine outlived its application owner");
}

#[test]
fn owned_engine_receives_the_same_review_settings_and_store_as_the_ui() {
    let (_root, paths) = fixture();
    let mut runtime = OwnedLocal::start(&paths, Duration::from_secs(2)).unwrap();
    let received: Vec<String> =
        serde_json::from_slice(&fs::read(paths.profile.join("home/review-paths")).unwrap())
            .unwrap();
    assert_eq!(
        received,
        vec![
            paths.review_config.to_str().unwrap(),
            paths.review_root.to_str().unwrap()
        ]
    );
    assert!(!paths.review_config.exists());
    assert!(!paths.review_root.exists());
    assert!(!paths.profile.join("home/capture").exists());
    runtime.shutdown().unwrap();
}
