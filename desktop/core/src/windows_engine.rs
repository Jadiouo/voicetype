//! App-owned Windows Nano child over inherited stdio. The child has no listener.
//! Its stdout reader owns framing; the resident desktop worker owns commands and
//! the process handle. EOF is failure, never a release acknowledgement.
use crate::{CommandRejected, Provider, ProviderCommand, ProviderEvent, ProviderPort,
    SessionFailure, SessionKey};
use serde_json::{json, Value};
use std::{fs::{File, OpenOptions}, io::{self, BufRead, BufReader, Read, Write}, path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio}, sync::mpsc::{self, Receiver},
    thread, time::Duration};

const MAX_FRAME: usize = 256 * 1024;

pub struct EnginePaths {
    pub executable: PathBuf,
    pub model_dir: PathBuf,
    pub vad_model: PathBuf,
    pub profile: PathBuf,
    pub vocabulary: PathBuf,
    pub review_config: PathBuf,
    pub review_root: PathBuf,
}

enum Frame { Value(Value), Fault }

pub struct WindowsEngine {
    child: Child,
    _profile_lock: File,
    #[cfg(windows)]
    job: Option<crate::windows_job::WindowsJob>,
    stdin: ChildStdin,
    frames: Receiver<Frame>,
    reader: Option<thread::JoinHandle<()>>,
    active: Option<SessionKey>,
    faulted: bool,
}

impl WindowsEngine {
    pub fn start(paths: EnginePaths) -> io::Result<Self> {
        for path in [&paths.executable, &paths.model_dir, &paths.vad_model,
                     &paths.profile, &paths.vocabulary, &paths.review_config, &paths.review_root] {
            if !path.is_absolute() { return Err(io::ErrorKind::InvalidInput.into()); }
        }
        if !paths.executable.is_file() || !paths.model_dir.is_dir() || !paths.vad_model.is_file() {
            return Err(io::ErrorKind::NotFound.into());
        }
        std::fs::create_dir_all(paths.profile.join("home"))?;
        std::fs::create_dir_all(paths.profile.join("config"))?;
        std::fs::create_dir_all(paths.profile.join("data"))?;
        std::fs::create_dir_all(&paths.review_root)?;
        let profile_lock = OpenOptions::new().read(true).write(true).create(true).truncate(false)
            .open(paths.profile.join("runtime.lock"))?;
        profile_lock.try_lock().map_err(|_| io::Error::new(io::ErrorKind::WouldBlock,
            "Windows local profile already owned"))?;
        let mut command = Command::new(&paths.executable);
        command
            .arg("--desktop-stdio")
            .env_clear()
            .env("VOICETYPE_ASR_PROFILE", "nano")
            .env("VOICETYPE_NANO_MODEL_DIR", &paths.model_dir)
            .env("VOICETYPE_NANO_VAD_MODEL", &paths.vad_model)
            .env("VOICETYPE_VOCAB", &paths.vocabulary)
            .env("VOICETYPE_REVIEW_CONFIG", &paths.review_config)
            .env("VOICETYPE_REVIEW_ROOT", &paths.review_root)
            .env("APPDATA", paths.profile.join("config"))
            .env("LOCALAPPDATA", paths.profile.join("data"))
            .env("USERPROFILE", &paths.profile)
            .env("HOME", paths.profile.join("home"))
            .env("XDG_CONFIG_HOME", paths.profile.join("config"))
            .env("XDG_DATA_HOME", paths.profile.join("data"))
            .env("VOICETYPE_LEARNING_FILE", paths.profile.join("data/learning.json"))
            .env("CUDA_VISIBLE_DEVICES", "")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        for name in ["SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(name) { command.env(name, value); }
        }
        let mut child = command.spawn()?;
        #[cfg(windows)]
        let job = match crate::windows_job::WindowsJob::attach(&child) {
            Ok(job) => Some(job),
            Err(error) => { let _ = child.kill(); let _ = child.wait(); return Err(error); }
        };
        let stdin = child.stdin.take().ok_or(io::ErrorKind::BrokenPipe)?;
        let stdout = child.stdout.take().ok_or(io::ErrorKind::BrokenPipe)?;
        let (tx, frames) = mpsc::sync_channel(64);
        let reader = thread::Builder::new().name("voicetype-win-engine-read".into())
            .spawn(move || {
                let mut source = BufReader::new(stdout);
                loop {
                    let mut frame = Vec::new();
                    // Limit the allocation even when a crashed child writes no newline.
                    match source.by_ref().take((MAX_FRAME + 1) as u64).read_until(b'\n', &mut frame) {
                        Ok(n) if n > 0 && n <= MAX_FRAME && frame.last() == Some(&b'\n') => {
                            match serde_json::from_slice(&frame) {
                                Ok(value) => { if tx.try_send(Frame::Value(value)).is_err() { break; } }
                                Err(_) => break,
                            }
                        }
                        _ => break,
                    }
                }
                let _ = tx.try_send(Frame::Fault);
            })?;
        let mut engine = Self { child, _profile_lock: profile_lock, #[cfg(windows)] job,
            stdin, frames, reader: Some(reader), active: None, faulted: false };
        engine.write(&json!({"type":"desktop_status","request":1}))?;
        let reply = engine.frames.recv_timeout(Duration::from_secs(30))
            .map_err(|_| io::ErrorKind::TimedOut)?;
        let Frame::Value(reply) = reply else { return Err(io::ErrorKind::BrokenPipe.into()); };
        let value = &reply["value"];
        if reply["type"] != "info" || value["desktop_protocol"] != 1 ||
            value["request"] != 1 || value["session_busy"] != false ||
            !value["capabilities"].as_array().is_some_and(|a| a.contains(&json!("session_events")) && a.contains(&json!("suspend"))) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "incompatible Nano child"));
        }
        Ok(engine)
    }

    fn write(&mut self, value: &Value) -> io::Result<()> {
        serde_json::to_writer(&mut self.stdin, value)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()
    }

    pub fn poll_event(&mut self) -> io::Result<Option<(SessionKey, ProviderEvent)>> {
        if self.faulted { return Err(io::ErrorKind::BrokenPipe.into()); }
        if self.child.try_wait()?.is_some() { self.faulted = true; return Err(io::ErrorKind::BrokenPipe.into()); }
        for _ in 0..32 {
            let frame = match self.frames.try_recv() {
                Ok(Frame::Value(value)) => value,
                Ok(Frame::Fault) | Err(mpsc::TryRecvError::Disconnected) => {
                    self.faulted = true;
                    return Err(io::ErrorKind::BrokenPipe.into());
                }
                Err(mpsc::TryRecvError::Empty) => return Ok(None),
            };
            let Some(key) = self.active else { continue; };
            if frame["session"].as_u64() != Some(key.id()) { continue; }
            let event = match frame["type"].as_str() {
                Some("state") if frame["value"] == "recording" => ProviderEvent::Recording,
                Some("state") if frame["value"] == "idle" => { self.active = None; ProviderEvent::Released },
                Some("result") => match frame["text"].as_str() {
                    Some(text) => ProviderEvent::Final(text.into()),
                    None => { self.faulted = true; return Err(io::ErrorKind::InvalidData.into()); }
                },
                Some("error") => ProviderEvent::Failed(SessionFailure::ProviderFailed),
                _ => { self.faulted = true; return Err(io::ErrorKind::InvalidData.into()); }
            };
            return Ok(Some((key, event)));
        }
        self.faulted = true;
        Err(io::Error::new(io::ErrorKind::InvalidData, "engine event burst"))
    }

    pub fn shutdown(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        self.job.take(); // Kill the entire child tree before waiting for direct child.
        if self.child.try_wait()?.is_none() {
            // The Job may reap the child between the status query and kill.
            // In that race a failed kill is harmless only after exit is proved.
            if let Err(error) = self.child.kill() {
                if self.child.try_wait()?.is_none() { return Err(error); }
            }
        }
        self.child.wait()?;
        if let Some(reader) = self.reader.take() { let _ = reader.join(); }
        Ok(())
    }
}

impl ProviderPort for WindowsEngine {
    fn send(&mut self, command: ProviderCommand) -> Result<(), CommandRejected> {
        if self.faulted { return Err(CommandRejected); }
        let frame = match command {
            ProviderCommand::Start { key, target, context } if key.provider() == Provider::Local && self.active.is_none() => {
                self.active = Some(key);
                json!({"type":"start","session":key.id(),"context_id":target.context_id,
                    "program":context.program,"context_text":context.context_text,
                    "selected_text":context.selected_text,"session_events":true})
            }
            ProviderCommand::Stop { key } if self.active == Some(key) => json!({"type":"stop","session":key.id()}),
            ProviderCommand::Cancel { key } if self.active == Some(key) => json!({"type":"cancel","session":key.id()}),
            _ => return Err(CommandRejected),
        };
        if self.write(&frame).is_err() { self.faulted = true; }
        // A partial write is uncertain: keep coordinator ownership until process reap.
        Ok(())
    }
}

impl Drop for WindowsEngine { fn drop(&mut self) { let _ = self.shutdown(); } }
