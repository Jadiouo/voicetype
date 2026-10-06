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
    };
    (root, paths)
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
    assert_eq!(directory.parent(), Some(std::path::Path::new("/tmp")));
    assert!(directory
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("voicetype-app-"));
    fs::remove_dir_all(directory).unwrap();
    assert!(exited, "engine outlived its application owner");
}
