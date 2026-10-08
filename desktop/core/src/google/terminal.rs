//! Bounded interactive PTY on Linux and ConPTY on Windows. Terminal output is
//! drained only to prevent blocking; it is never used as transcript text.
#[cfg(windows)]
use crate::windows_job::WindowsJob;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 256 * 1024;

pub struct TerminalLaunch {
    pub executable: PathBuf,
    pub sha256: String,
    pub workspace: PathBuf,
    pub profile: PathBuf,
    pub editor: PathBuf,
}

/// No generic key or prompt-writing API exists. Enter is deliberately absent.
pub enum TerminalControl {
    VoiceToggle,
    EditDraft,
    Discard,
}

impl TerminalControl {
    fn bytes(&self) -> &'static [u8] {
        match self {
            Self::VoiceToggle => b"\x1b[15~",
            Self::EditDraft => b"\x07",
            Self::Discard => b"\x1b",
        }
    }
}

pub struct GoogleTerminal {
    master: Option<Box<dyn MasterPty + Send>>,
    child: Option<Box<dyn Child + Send + Sync>>,
    #[cfg(windows)]
    job: Option<WindowsJob>,
    writer: Box<dyn Write + Send>,
    reader: Option<JoinHandle<()>>,
    reader_done: Arc<AtomicBool>,
    output_bytes: Arc<AtomicUsize>,
    session: tempfile::TempDir,
    captures: usize,
}

impl GoogleTerminal {
    pub fn launch(config: TerminalLaunch) -> io::Result<Self> {
        Self::launch_with_env(config, &[])
    }

    pub(crate) fn launch_with_env(
        config: TerminalLaunch,
        native_audio_env: &[(OsString, OsString)],
    ) -> io::Result<Self> {
        verify_binary(&config.executable, &config.sha256)?;
        if !config.workspace.is_absolute()
            || !config.workspace.is_dir()
            || !config.profile.is_absolute()
            || !config.profile.is_dir()
            || !config.editor.is_absolute()
            || !config.editor.is_file()
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let session = tempfile::Builder::new()
            .prefix("google-")
            .tempdir_in(&config.profile)?;
        let temporary = session.path().join("tmp");
        let capture = session.path().join("capture");
        for dir in [&temporary, &capture] {
            fs::create_dir(dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
            }
        }
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 40,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io::Error::other)?;
        let mut command = CommandBuilder::new(config.executable.as_os_str());
        command.env_clear();
        command.arg("--log-file");
        command.arg(session.path().join("official-cli.log").as_os_str());
        command.cwd(config.workspace.as_os_str());
        for key in [
            "HOME",
            "USER",
            "LOGNAME",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XAUTHORITY",
            "DBUS_SESSION_BUS_ADDRESS",
            "XDG_RUNTIME_DIR",
            "COLORTERM",
            "SystemRoot",
            "WINDIR",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "SYSTEMDRIVE",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command.env("TERM", "xterm-256color");
        command.env("LANG", "C.UTF-8");
        command.env("AGY_CLI_DISABLE_AUTO_UPDATE", "true");
        command.env("TMPDIR", temporary.as_os_str());
        command.env("TEMP", temporary.as_os_str());
        command.env("TMP", temporary.as_os_str());
        command.env("VOICETYPE_GOOGLE_TMP", temporary.as_os_str());
        command.env("VOICETYPE_GOOGLE_CAPTURE", capture.as_os_str());
        command.env("EDITOR", quoted_editor(&config.editor));
        command.env("VISUAL", quoted_editor(&config.editor));
        #[cfg(windows)]
        command.env(
            "PATH",
            std::env::var_os("SystemRoot")
                .map(|root| PathBuf::from(root).join("System32").into_os_string())
                .unwrap_or_default(),
        );
        #[cfg(unix)]
        command.env("PATH", "/usr/bin:/bin");
        for (key, value) in native_audio_env {
            command.env(key, value);
        }
        #[allow(unused_mut)] // Windows failure path must kill and wait the child.
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(io::Error::other)?;
        #[cfg(windows)]
        let job = {
            let attached = child
                .as_raw_handle()
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))
                .and_then(|handle| unsafe { WindowsJob::attach_raw_handle(handle) });
            match attached {
                Ok(job) => job,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            }
        };
        drop(pair.slave);
        let writer = pair.master.take_writer().map_err(io::Error::other)?;
        let mut output = pair.master.try_clone_reader().map_err(io::Error::other)?;
        let output_bytes = Arc::new(AtomicUsize::new(0));
        let reader_done = Arc::new(AtomicBool::new(false));
        let count = output_bytes.clone();
        let done = reader_done.clone();
        let reader = thread::Builder::new()
            .name("voicetype-google-pty".into())
            .spawn(move || {
                let mut buffer = [0u8; 8192];
                while let Ok(n) = output.read(&mut buffer) {
                    if n == 0 {
                        break;
                    }
                    count.fetch_add(n, Ordering::AcqRel);
                }
                done.store(true, Ordering::Release);
            })?;
        Ok(Self {
            master: Some(pair.master),
            child: Some(child),
            #[cfg(windows)]
            job: Some(job),
            writer,
            reader: Some(reader),
            reader_done,
            output_bytes,
            session,
            captures: 0,
        })
    }

    pub fn control(&mut self, control: TerminalControl) -> io::Result<()> {
        if self.output_bytes.load(Ordering::Acquire) > OUTPUT_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "CLI output limit",
            ));
        }
        if self
            .child
            .as_mut()
            .ok_or(io::ErrorKind::BrokenPipe)?
            .try_wait()?
            .is_some()
        {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        self.writer.write_all(control.bytes())?;
        self.writer.flush()
    }

    pub fn capture_dir(&self) -> PathBuf {
        self.session.path().join("capture")
    }
    pub fn log_path(&self) -> PathBuf {
        self.session.path().join("official-cli.log")
    }

    pub fn capture_editor(&mut self, budget: Duration) -> io::Result<String> {
        if self.captures >= 3 || budget.is_zero() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let number = self.captures + 1;
        let path = self.capture_dir().join(format!("draft-{number}.json"));
        let mut stage = OpenOptions::new();
        stage.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            stage.mode(0o600);
        }
        let mut stage = stage.open(self.capture_dir().join("active-stage"))?;
        stage.write_all(number.to_string().as_bytes())?;
        stage.sync_all()?;
        let until = Instant::now() + budget.min(Duration::from_secs(18));
        let mut next_control = Instant::now();
        loop {
            if Instant::now() >= next_control {
                self.control(TerminalControl::EditDraft)?;
                next_control = Instant::now() + Duration::from_millis(200);
            }
            match read_capture(&path, number) {
                Ok(Some(text)) => {
                    self.captures = number;
                    return Ok(text);
                }
                Ok(None) if Instant::now() < until => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => return Err(io::ErrorKind::TimedOut.into()),
                Err(error) => return Err(error),
            }
        }
    }

    pub fn shutdown(&mut self) -> io::Result<()> {
        if let Some(child) = self.child.as_mut() {
            #[cfg(unix)]
            if let Some(group) = self
                .master
                .as_ref()
                .and_then(|master| master.process_group_leader())
            {
                if group > 0 {
                    unsafe {
                        libc::kill(-group, libc::SIGTERM);
                    }
                }
            }
            if child.try_wait()?.is_none() {
                child.kill()?;
            }
            let until = Instant::now() + Duration::from_millis(500);
            while child.try_wait()?.is_none() {
                if Instant::now() >= until {
                    #[cfg(unix)]
                    if let Some(group) = self
                        .master
                        .as_ref()
                        .and_then(|master| master.process_group_leader())
                    {
                        if group > 0 {
                            unsafe {
                                libc::kill(-group, libc::SIGKILL);
                            }
                        }
                    }
                    child.kill()?;
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            child.wait()?;
            self.child.take();
        }
        #[cfg(windows)]
        self.job.take(); // closes Job Object and kills any remaining children
        self.master.take();
        let until = Instant::now() + Duration::from_millis(500);
        while !self.reader_done.load(Ordering::Acquire) && Instant::now() < until {
            thread::sleep(Duration::from_millis(5));
        }
        if !self.reader_done.load(Ordering::Acquire) {
            return Err(io::ErrorKind::TimedOut.into());
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        Ok(())
    }
}

fn read_capture(path: &PathBuf, number: usize) -> io::Result<Option<String>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !meta.file_type().is_file() || meta.len() > 256 * 1024 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
    }
    let bytes = fs::read(path)?;
    if bytes.len() > 256 * 1024 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return Ok(None), // helper may still be writing its exclusive file
    };
    let text = value["text"].as_str().ok_or(io::ErrorKind::InvalidData)?;
    if value["stage"].as_u64() != Some(number as u64)
        || value["agent_prompt_submitted"] != false
        || value["source_modified"] != false
        || value["bytes"].as_u64() != Some(text.len() as u64)
        || value["sha256"].as_str() != Some(&format!("{:x}", Sha256::digest(text.as_bytes())))
        || text.len() > 64 * 1024
        || text.contains('\0')
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(Some(text.into()))
}

impl Drop for GoogleTerminal {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn quoted_editor(path: &PathBuf) -> String {
    format!("\"{}\"", path.display())
}

fn verify_binary(path: &PathBuf, expected: &str) -> io::Result<()> {
    if !path.is_absolute()
        || expected.len() != 64
        || !expected.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    io::copy(&mut file, &mut hash)?;
    if format!("{:x}", hash.finalize()) != expected.to_ascii_lowercase() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unreviewed official CLI binary",
        ));
    }
    Ok(())
}
