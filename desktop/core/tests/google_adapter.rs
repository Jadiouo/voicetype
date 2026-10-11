#![cfg(target_os = "linux")]

use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use voicetype_app_core::google::{
    adapter::{GoogleAudio, GoogleCliBoundary},
    native::GoogleNativeBoundary,
    terminal::TerminalLaunch,
    GoogleAttempt, GoogleBoundary, GoogleCaptureOwnership,
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

#[test]
fn official_native_cli_owns_capture_and_never_returns_review_pcm() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("fake-official-cli");
    let controls = root.path().join("controls");
    let code = format!(
        r#"#!/usr/bin/python3 -u
import os,subprocess,shlex,tty,sys
tty.setraw(0)
open(sys.argv[sys.argv.index('--log-file')+1],'wb').close()
count=0
voiced=False
while True:
 ch=os.read(0,1)
 with open({:?},'ab') as out: out.write(ch)
 if ch==b'~' and not voiced:
  voiced=True
  os.write(1,b'Connecting...\r\xe2\x97\x8f Recording 0:00  enter or f5 to insert \xc2\xb7 esc to discard')
 if ch==b'\x07':
  text=['','早期','完整句子'][count]
  count+=1
  path=os.path.join(os.environ['VOICETYPE_GOOGLE_TMP'],'prompt')
  with open(path,'w') as out: out.write(text)
  os.chmod(path,0o600)
  subprocess.run(shlex.split(os.environ['EDITOR'])+[path],check=True)
"#,
        controls.display().to_string()
    );
    fs::write(&script, code).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let mut boundary = GoogleNativeBoundary::open(
        TerminalLaunch {
            executable: script.clone(),
            sha256: format!("{:x}", Sha256::digest(fs::read(&script).unwrap())),
            workspace,
            profile: root.path().to_path_buf(),
            editor: env!("CARGO_BIN_EXE_voicetype-google-editor").into(),
        },
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(
        boundary.capture_ownership(),
        GoogleCaptureOwnership::OfficialCliNative
    );
    let mut attempt = GoogleAttempt::start(&mut boundary).unwrap();
    assert_eq!(attempt.finish(&mut boundary).unwrap(), "完整句子");
    assert_eq!(boundary.take_review_pcm(), None);
    assert_eq!(
        fs::read(&controls).unwrap(),
        b"\x07\x1b[15~\x1b[15~\x07\x07"
    );
    boundary.prewarm_next().unwrap();
    let mut second = GoogleAttempt::start(&mut boundary).unwrap();
    assert_eq!(second.finish(&mut boundary).unwrap(), "完整句子");
    assert_eq!(
        fs::read(controls).unwrap(),
        b"\x07\x1b[15~\x1b[15~\x07\x07\x07\x1b[15~\x1b[15~\x07\x07"
    );
}

#[test]
fn native_f5_without_recording_status_or_after_voice_error_never_reports_ready() {
    for error_after_connecting in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("fake-official-cli");
        let code = format!(
            r#"#!/usr/bin/python3 -u
import os,subprocess,shlex,tty,sys
tty.setraw(0)
log=sys.argv[sys.argv.index('--log-file')+1]
open(log,'wb').close()
while True:
 ch=os.read(0,1)
 if ch==b'\x07':
  path=os.path.join(os.environ['VOICETYPE_GOOGLE_TMP'],'prompt')
  open(path,'w').close()
  os.chmod(path,0o600)
  subprocess.run(shlex.split(os.environ['EDITOR'])+[path],check=True)
 elif ch==b'~':
  os.write(1,b'Connecting...')
  if {}:
   with open(log,'ab') as out: out.write(b'voice: error microphone denied\n')
"#,
            usize::from(error_after_connecting)
        );
        fs::write(&script, code).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let mut boundary = GoogleNativeBoundary::open(
            TerminalLaunch {
                executable: script.clone(),
                sha256: format!("{:x}", Sha256::digest(fs::read(&script).unwrap())),
                workspace,
                profile: root.path().to_path_buf(),
                editor: env!("CARGO_BIN_EXE_voicetype-google-editor").into(),
            },
            Duration::from_secs(1),
        )
        .unwrap();
        let error = GoogleAttempt::start(&mut boundary).err().unwrap();
        assert_eq!(
            error.kind(),
            if error_after_connecting {
                std::io::ErrorKind::InvalidData
            } else {
                std::io::ErrorKind::TimedOut
            }
        );
        assert_eq!(
            boundary.start_capture().unwrap_err().kind(),
            std::io::ErrorKind::BrokenPipe
        );
    }
}

#[test]
fn failed_native_preflight_reaps_unpublished_cli() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("fake-official-cli");
    let pid_file = root.path().join("cli.pid");
    let code = format!(
        r#"#!/usr/bin/python3 -u
import os,sys,tty
tty.setraw(0)
pidfile={:?}
with open(pidfile+'.tmp','w') as out: out.write(str(os.getpid()))
os.replace(pidfile+'.tmp',pidfile)
while True: os.read(0,1)
"#,
        pid_file.display().to_string()
    );
    fs::write(&script, code).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let error = GoogleNativeBoundary::open(
        TerminalLaunch {
            executable: script.clone(),
            sha256: format!("{:x}", Sha256::digest(fs::read(&script).unwrap())),
            workspace,
            profile: root.path().to_path_buf(),
            editor: env!("CARGO_BIN_EXE_voicetype-google-editor").into(),
        },
        Duration::from_millis(200),
    )
    .err()
    .unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    let pid: u32 = fs::read_to_string(&pid_file).unwrap().parse().unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(3);
    while std::path::Path::new(&format!("/proc/{pid}")).exists()
        && std::time::Instant::now() < until
    {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}

#[test]
fn native_connecting_cancel_never_becomes_ready_and_reaps_boundary() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("fake-official-cli");
    let requested = root.path().join("requested");
    let code = format!(
        r#"#!/usr/bin/python3 -u
import os,subprocess,shlex,tty,sys
tty.setraw(0)
open(sys.argv[sys.argv.index('--log-file')+1],'wb').close()
while True:
 ch=os.read(0,1)
 if ch==b'\x07':
  path=os.path.join(os.environ['VOICETYPE_GOOGLE_TMP'],'prompt')
  open(path,'w').close()
  os.chmod(path,0o600)
  subprocess.run(shlex.split(os.environ['EDITOR'])+[path],check=True)
 elif ch==b'~':
  open({:?},'wb').close()
  os.write(1,b'Connecting...')
"#,
        requested.display().to_string()
    );
    fs::write(&script, code).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let mut boundary = GoogleNativeBoundary::open(
        TerminalLaunch {
            executable: script.clone(),
            sha256: format!("{:x}", Sha256::digest(fs::read(&script).unwrap())),
            workspace,
            profile: root.path().to_path_buf(),
            editor: env!("CARGO_BIN_EXE_voicetype-google-editor").into(),
        },
        Duration::from_secs(1),
    )
    .unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    boundary.set_cancellation(cancelled.clone());
    let signal = thread::spawn(move || {
        for _ in 0..200 {
            if requested.exists() {
                cancelled.store(true, Ordering::Release);
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        false
    });
    assert_eq!(
        GoogleAttempt::start(&mut boundary).err().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(signal.join().unwrap());
    assert_eq!(
        boundary.start_capture().unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
}
