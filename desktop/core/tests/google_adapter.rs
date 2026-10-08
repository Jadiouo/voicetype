#![cfg(target_os = "linux")]

use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::Duration,
};
use voicetype_app_core::google::{
    adapter::{GoogleAudio, GoogleCliBoundary},
    terminal::TerminalLaunch,
    GoogleAttempt,
};

#[derive(Clone)]
struct Audio(Arc<Mutex<Vec<&'static str>>>);

impl GoogleAudio for Audio {
    fn start(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().push("start");
        Ok(())
    }
    fn wait_connected(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().push("connected");
        Ok(())
    }
    fn stop_and_drain(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().push("drained");
        Ok(())
    }
    fn wait_recorder_exit(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().push("recorder_exit");
        Ok(())
    }
    fn cleanup(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().push("cleanup");
        Ok(())
    }
}

#[test]
fn concrete_cli_adapter_preflights_then_drains_before_one_stop_and_two_captures() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("fake-official-cli");
    let controls = root.path().join("controls");
    let code = format!(
        r#"#!/usr/bin/python3 -u
import os,subprocess,shlex,tty,sys
tty.setraw(0)
open(sys.argv[sys.argv.index('--log-file')+1],'wb').close()
count=0
while True:
 ch=os.read(0,1)
 if ch==b'\x07':
  text=['','早期','完整句子'][count]
  count+=1
  path=os.path.join(os.environ['VOICETYPE_GOOGLE_TMP'],'prompt')
  with open(path,'w') as out: out.write(text)
  os.chmod(path,0o600)
  subprocess.run(shlex.split(os.environ['EDITOR'])+[path],check=True)
 elif ch==b'~':
  with open({:?},'ab') as out: out.write(b'F5')
"#,
        controls.display().to_string()
    );
    fs::write(&script, code).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let steps = Arc::new(Mutex::new(Vec::new()));
    let mut boundary = GoogleCliBoundary::open(
        TerminalLaunch {
            executable: script.clone(),
            sha256: format!("{:x}", Sha256::digest(fs::read(&script).unwrap())),
            workspace,
            profile: root.path().to_path_buf(),
            editor: env!("CARGO_BIN_EXE_voicetype-google-editor").into(),
        },
        Audio(steps.clone()),
        Duration::from_secs(1),
    )
    .unwrap();
    let mut attempt = GoogleAttempt::start(&mut boundary).unwrap();
    assert_eq!(attempt.finish(&mut boundary).unwrap(), "完整句子");
    assert_eq!(
        *steps.lock().unwrap(),
        ["start", "connected", "drained", "recorder_exit", "cleanup"]
    );
    assert_eq!(fs::read(controls).unwrap(), b"F5F5");
}
