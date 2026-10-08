#![cfg(target_os = "linux")]

use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, thread, time::Duration};
use voicetype_app_core::google::terminal::{GoogleTerminal, TerminalControl, TerminalLaunch};

#[test]
fn private_pty_only_writes_voice_and_editor_controls_never_enter() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("fake-official-cli");
    let output = root.path().join("controls.bin");
    let ready = root.path().join("ready");
    fs::write(&script, format!("#!/usr/bin/python3 -u\nimport os,tty\ntty.setraw(0)\nopen({:?},'wb').close()\ndata=b''\nwhile len(data)<6:\n data+=os.read(0,6-len(data))\nopen({:?},'wb').write(data)\n", ready.display().to_string(), output.display().to_string())).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let hash = format!("{:x}", Sha256::digest(fs::read(&script).unwrap()));
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let mut terminal = GoogleTerminal::launch(TerminalLaunch {
        executable: script,
        sha256: hash,
        workspace,
        profile: root.path().to_path_buf(),
        editor: std::env::current_exe().unwrap(),
    })
    .unwrap();
    for _ in 0..100 {
        if ready.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(ready.exists());
    terminal.control(TerminalControl::VoiceToggle).unwrap();
    terminal.control(TerminalControl::EditDraft).unwrap();
    for _ in 0..100 {
        if output.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fs::read(output).unwrap(), b"\x1b[15~\x07");
    terminal.shutdown().unwrap();
}

#[test]
fn official_editor_hook_supplies_empty_handshake_and_two_later_snapshots() {
    let root = tempfile::tempdir().unwrap();
    let ready = root.path().join("ready");
    let script = root.path().join("fake-official-cli");
    fs::write(
        &script,
        format!(
            r#"#!/usr/bin/python3 -u
import os,subprocess,shlex,tty
tty.setraw(0)
open({:?},'wb').close()
index=0
while True:
 ch=os.read(0,1)
 if ch==b'\x07':
  text=['','早期','完整句子 GitHub'][index]
  index+=1
  path=os.path.join(os.environ['VOICETYPE_GOOGLE_TMP'],'prompt')
  with open(path,'w') as out: out.write(text)
  os.chmod(path,0o600)
  subprocess.run(shlex.split(os.environ['EDITOR'])+[path],check=True)
"#,
            ready.display().to_string()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let mut terminal = GoogleTerminal::launch(TerminalLaunch {
        sha256: format!("{:x}", Sha256::digest(fs::read(&script).unwrap())),
        executable: script,
        workspace,
        profile: root.path().to_path_buf(),
        editor: env!("CARGO_BIN_EXE_voicetype-google-editor").into(),
    })
    .unwrap();
    for _ in 0..100 {
        if ready.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(ready.exists());
    assert_eq!(terminal.capture_editor(Duration::from_secs(1)).unwrap(), "");
    assert_eq!(
        terminal.capture_editor(Duration::from_secs(1)).unwrap(),
        "早期"
    );
    assert_eq!(
        terminal.capture_editor(Duration::from_secs(1)).unwrap(),
        "完整句子 GitHub"
    );
    terminal.shutdown().unwrap();
}
