#![cfg(windows)]

use sha2::{Digest, Sha256};
use std::{fs, sync::mpsc, thread, time::Duration};
use voicetype_app_core::google::terminal::{GoogleTerminal, TerminalControl, TerminalLaunch};

#[test]
fn conpty_delivers_only_voice_and_editor_controls_to_child() {
    let (progress, observed) = mpsc::channel();
    let case = thread::spawn(move || run_conpty_case(progress));
    let mut phase = "launch";
    loop {
        match observed.recv_timeout(Duration::from_secs(8)) {
            Ok("done") => {
                case.join().unwrap();
                break;
            }
            Ok(next) => phase = next,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("ConPTY fixture exceeded its phase deadline at {phase}")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let panicked = case.join().is_err();
                panic!("ConPTY fixture exited before completion at {phase}; panicked={panicked}")
            }
        }
    }
}

fn run_conpty_case(progress: mpsc::Sender<&'static str>) {
    let mark = |phase| {
        let _ = progress.send(phase);
    };
    mark("launch");
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let executable = std::path::PathBuf::from(env!("CARGO_BIN_EXE_voicetype-google-pty-fixture"));
    let mut terminal = GoogleTerminal::launch(TerminalLaunch {
        sha256: format!("{:x}", Sha256::digest(fs::read(&executable).unwrap())),
        executable,
        workspace: workspace.clone(),
        profile: root.path().to_path_buf(),
        editor: env!("CARGO_BIN_EXE_voicetype-google-editor").into(),
    })
    .unwrap();
    mark("ready");
    for _ in 0..200 {
        if workspace.join("ready").exists() {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(workspace.join("ready").exists());
    mark("initial_editor");
    assert_eq!(terminal.capture_editor(Duration::from_secs(2)).unwrap(), "");
    mark("first_f5");
    terminal.control(TerminalControl::VoiceToggle).unwrap();
    mark("second_f5");
    terminal.control(TerminalControl::VoiceToggle).unwrap();
    mark("early_editor");
    assert_eq!(
        terminal.capture_editor(Duration::from_secs(2)).unwrap(),
        "早期"
    );
    mark("final_editor");
    assert_eq!(
        terminal.capture_editor(Duration::from_secs(2)).unwrap(),
        "完整句子"
    );
    mark("controls");
    let controls = fs::read(workspace.join("controls")).unwrap();
    assert_eq!(controls.iter().filter(|byte| **byte == b'~').count(), 2);
    assert!(!controls.contains(&b'\r'));
    mark(if terminal.child_exit_observed().unwrap() {
        "shutdown_exit_observed"
    } else {
        "shutdown_exit_unconfirmed"
    });
    terminal.shutdown().unwrap();
    drop(terminal);
    drop(root);
    mark("done");
}
