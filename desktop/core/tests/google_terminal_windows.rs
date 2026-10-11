#![cfg(windows)]

use sha2::{Digest, Sha256};
use std::{fs, thread, time::Duration};
use voicetype_app_core::google::terminal::{GoogleTerminal, TerminalControl, TerminalLaunch};

#[test]
fn conpty_delivers_only_voice_and_editor_controls_to_child() {
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
    for _ in 0..200 {
        if workspace.join("ready").exists() {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(workspace.join("ready").exists());
    assert_eq!(terminal.capture_editor(Duration::from_secs(2)).unwrap(), "");
    terminal.control(TerminalControl::VoiceToggle).unwrap();
    terminal.control(TerminalControl::VoiceToggle).unwrap();
    assert_eq!(
        terminal.capture_editor(Duration::from_secs(2)).unwrap(),
        "早期"
    );
    assert_eq!(
        terminal.capture_editor(Duration::from_secs(2)).unwrap(),
        "完整句子"
    );
    let controls = fs::read(workspace.join("controls")).unwrap();
    assert_eq!(controls.iter().filter(|byte| **byte == b'~').count(), 2);
    assert!(!controls.contains(&b'\r'));
    terminal.shutdown().unwrap();
}
