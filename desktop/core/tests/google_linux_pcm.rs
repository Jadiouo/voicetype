#![cfg(target_os = "linux")]

use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, thread, time::Duration};
use voicetype_app_core::google::{
    adapter::{GoogleAudio, GoogleCliBoundary},
    linux_audio::LinuxPcmAudio,
    terminal::TerminalLaunch,
    GoogleAttempt, GoogleBoundary, GoogleProvider,
};
use voicetype_app_core::{Application, Provider, ProviderEvent, TargetLease};

#[test]
fn synthetic_native_pcm_is_consumed_before_official_stop_and_delivery() {
    let root = tempfile::tempdir().unwrap();
    let recorder = root.path().join("fake-pw-record-native");
    let generated = root.path().join("generated-count");
    fs::write(
        &recorder,
        format!(
            r#"#!/usr/bin/python3 -u
import os,signal,time
n=0
stop=False
def quit(sig,frame):
 global stop
 stop=True
signal.signal(signal.SIGINT,quit)
while not stop:
 os.write(1,b'\x01\x00'*320)
 n+=640
 time.sleep(.02)
os.write(1,b'\x02\x00'*160)
n+=320
open({:?},'w').write(str(n))
"#,
            generated.display().to_string()
        ),
    )
    .unwrap();
    fs::set_permissions(&recorder, fs::Permissions::from_mode(0o700)).unwrap();

    let cli = root.path().join("fake-official-cli");
    let consumed = root.path().join("consumed-count");
    fs::write(&cli, format!(r#"#!/usr/bin/python3 -u
import os,select,subprocess,shlex,threading,tty,sys
tty.setraw(0)
open(sys.argv[sys.argv.index('--log-file')+1],'wb').close()
child=None
done=threading.Event()
seen=[0]
def read_pcm():
 fd=child.stdout.fileno()
 while not done.is_set():
  if not select.select([fd],[],[],.01)[0]: continue
  data=os.read(fd,65536)
  if not data: return
  seen[0]+=len(data)
edits=0
voice=0
while True:
 ch=os.read(0,1)
 if ch==b'\x07':
  text=['','','語音完整'][edits]
  edits+=1
  path=os.path.join(os.environ['VOICETYPE_GOOGLE_TMP'],'prompt')
  with open(path,'w') as out: out.write(text)
  os.chmod(path,0o600)
  subprocess.run(shlex.split(os.environ['EDITOR'])+[path],check=True)
 elif ch==b'~':
  voice+=1
  if voice==1:
   child=subprocess.Popen(['pw-record','--rate=16000','--channels=1','--format=s16','--raw','-'],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
   threading.Thread(target=read_pcm,daemon=True).start()
  elif voice==2:
   done.set()
   child.stdout.close()
   child.wait(timeout=2)
   open({:?},'w').write(str(seen[0]))
"#, consumed.display().to_string())).unwrap();
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();

    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let audio = LinuxPcmAudio::new(
        root.path(),
        &recorder,
        &format!("{:x}", Sha256::digest(fs::read(&recorder).unwrap())),
        std::path::Path::new(env!("CARGO_BIN_EXE_voicetype-google-pcm")),
    )
    .unwrap();
    let mut boundary = GoogleCliBoundary::open(
        TerminalLaunch {
            executable: cli.clone(),
            sha256: format!("{:x}", Sha256::digest(fs::read(&cli).unwrap())),
            workspace,
            profile: root.path().to_path_buf(),
            editor: env!("CARGO_BIN_EXE_voicetype-google-editor").into(),
        },
        audio,
        Duration::from_secs(2),
    )
    .unwrap();
    let mut attempt = GoogleAttempt::start(&mut boundary).unwrap();
    thread::sleep(Duration::from_millis(130));
    assert_eq!(attempt.finish(&mut boundary).unwrap(), "語音完整");
    let produced: usize = fs::read_to_string(generated).unwrap().parse().unwrap();
    let accepted: usize = fs::read_to_string(consumed).unwrap().parse().unwrap();
    assert!(produced >= 640 + 320);
    assert_eq!(accepted, produced);
}

#[test]
fn reviewed_pipewire_symlink_is_accepted_without_starting_a_microphone() {
    let root = tempfile::tempdir().unwrap();
    let recorder = std::path::Path::new("/usr/bin/pw-record");
    if !recorder.exists() {
        return;
    }
    let shim = std::path::Path::new(env!("CARGO_BIN_EXE_voicetype-google-pcm"));
    let expected = "3874ae059c9eafdbd4d6fde4d4b7553aa83fe4149246e427a3208bdc4c712c64";
    if format!("{:x}", Sha256::digest(fs::read(recorder).unwrap())) != expected {
        return;
    }
    let audio = LinuxPcmAudio::new(root.path(), recorder, expected, shim).unwrap();
    assert!(!audio.catchup_enabled());
}

#[test]
fn dropping_a_started_native_recorder_kills_and_reaps_the_child() {
    let root = tempfile::tempdir().unwrap();
    let recorder = root.path().join("fake-recorder");
    let pid_file = root.path().join("recorder-pid");
    fs::write(
        &recorder,
        format!(
            r#"#!/usr/bin/python3 -u
import os,time
open({:?},'w').write(str(os.getpid()))
while True: time.sleep(1)
"#,
            pid_file.display().to_string()
        ),
    )
    .unwrap();
    fs::set_permissions(&recorder, fs::Permissions::from_mode(0o700)).unwrap();
    let shim = std::path::Path::new(env!("CARGO_BIN_EXE_voicetype-google-pcm"));
    let mut audio = LinuxPcmAudio::new(
        root.path(),
        &recorder,
        &format!("{:x}", Sha256::digest(fs::read(&recorder).unwrap())),
        shim,
    )
    .unwrap();
    audio.start().unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while !pid_file.is_file() && std::time::Instant::now() < until {
        thread::sleep(Duration::from_millis(5));
    }
    let pid: i32 = fs::read_to_string(&pid_file).unwrap().parse().unwrap();
    drop(audio);
    let result = unsafe { libc::kill(pid, 0) };
    assert_eq!(result, -1, "native recorder child outlived its owner");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[test]
fn failed_cleanup_retains_a_real_recorder_until_retry_reaps_it() {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    struct FailingAudio {
        audio: LinuxPcmAudio,
        allow: Arc<AtomicBool>,
        calls: Arc<AtomicUsize>,
    }
    impl GoogleBoundary for FailingAudio {
        fn start_capture(&mut self) -> std::io::Result<()> {
            self.audio.start()
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
            self.calls.fetch_add(1, Ordering::AcqRel);
            if self.allow.load(Ordering::Acquire) {
                self.audio.cleanup()
            } else {
                Err(std::io::ErrorKind::TimedOut.into())
            }
        }
    }
    let root = tempfile::tempdir().unwrap();
    let recorder = root.path().join("fake-recorder");
    let pid_file = root.path().join("recorder-pid");
    fs::write(
        &recorder,
        format!(
            r#"#!/usr/bin/python3 -u
import os,time
open({:?},'w').write(str(os.getpid()))
while True: time.sleep(1)
"#,
            pid_file.display().to_string()
        ),
    )
    .unwrap();
    fs::set_permissions(&recorder, fs::Permissions::from_mode(0o700)).unwrap();
    let audio = LinuxPcmAudio::new(
        root.path(),
        &recorder,
        &format!("{:x}", Sha256::digest(fs::read(&recorder).unwrap())),
        std::path::Path::new(env!("CARGO_BIN_EXE_voicetype-google-pcm")),
    )
    .unwrap();
    let allow = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut provider = GoogleProvider::spawn(FailingAudio {
        audio,
        allow: allow.clone(),
        calls: calls.clone(),
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
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while !pid_file.is_file() && std::time::Instant::now() < until {
        thread::sleep(Duration::from_millis(5));
    }
    let pid: i32 = fs::read_to_string(&pid_file).unwrap().parse().unwrap();
    app.cancel_dictation(&mut provider).unwrap();
    assert!(matches!(
        provider.recv_timeout(Duration::from_secs(1)),
        Some((_, ProviderEvent::Failed(_)))
    ));
    let drop_thread = thread::spawn(move || drop(provider));
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while calls.load(Ordering::Acquire) < 3 && std::time::Instant::now() < until {
        thread::sleep(Duration::from_millis(5));
    }
    let retried = calls.load(Ordering::Acquire) >= 3;
    let alive_while_failing = unsafe { libc::kill(pid, 0) } == 0;
    allow.store(true, Ordering::Release);
    drop_thread.join().unwrap();
    assert!(retried);
    assert!(
        alive_while_failing,
        "failed cleanup lost the recorder process"
    );
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "successful retry did not reap recorder"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}
