#![cfg(target_os = "linux")]

use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, thread, time::Duration};
use voicetype_app_core::google::{
    adapter::GoogleCliBoundary, linux_audio::LinuxPcmAudio, terminal::TerminalLaunch, GoogleAttempt,
};

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
