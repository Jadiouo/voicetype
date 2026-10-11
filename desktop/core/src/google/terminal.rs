//! Bounded interactive PTY on Linux and ConPTY on Windows. Terminal output is
//! drained only to prevent blocking; it is never used as transcript text.
use super::status::VoiceStatusProbe;
#[cfg(windows)]
use crate::windows_job::WindowsJob;
#[cfg(windows)]
use portable_pty::PtyPair;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use sha2::{Digest, Sha256};
#[cfg(windows)]
use std::sync::mpsc;
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 256 * 1024;
// A failed preflight can retire a CLI before any GoogleBoundary owns it. A
// later prewarm failure must not release the shared hook while that child or
// ConPTY still has an unconfirmed close. Spawn failure leaves this nonzero.
#[cfg(windows)]
static UNPUBLISHED_CLEANUPS: AtomicUsize = AtomicUsize::new(0);

#[cfg(windows)]
pub(crate) fn unpublished_cleanup_pending() -> bool {
    UNPUBLISHED_CLEANUPS.load(Ordering::Acquire) != 0
}

#[derive(Clone)]
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
    #[cfg(windows)]
    closing: Option<mpsc::Receiver<()>>,
    #[cfg(windows)]
    closed: bool,
    writer: Option<Box<dyn Write + Send>>,
    reader: Option<JoinHandle<()>>,
    reader_done: Arc<AtomicBool>,
    output_bytes: Arc<AtomicUsize>,
    voice_status: Arc<Mutex<VoiceStatusProbe>>,
    session: Arc<tempfile::TempDir>,
    captures: usize,
}

impl GoogleTerminal {
    /// A failed preflight has no provider owner yet. Keep the terminal alive
    /// off the resident thread until its child and ConPTY close are observed.
    pub(crate) fn retire_unpublished(self) {
        #[cfg(windows)]
        UNPUBLISHED_CLEANUPS.fetch_add(1, Ordering::AcqRel);
        let owner = Arc::new(Mutex::new(Some(self)));
        let retry = owner.clone();
        let spawned = thread::Builder::new()
            .name("voicetype-google-preflight-cleanup".into())
            .spawn(move || {
                let mut terminal = retry.lock().unwrap().take().unwrap();
                while !matches!(
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| terminal.shutdown())),
                    Ok(Ok(()))
                ) {
                    thread::sleep(Duration::from_millis(50));
                }
                #[cfg(windows)]
                UNPUBLISHED_CLEANUPS.fetch_sub(1, Ordering::AcqRel);
            });
        if spawned.is_err() {
            // Preserve the only process/ConPTY owner if retry cannot start.
            std::mem::forget(owner);
        }
    }

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
        let session = Arc::new(
            tempfile::Builder::new()
                .prefix("google-")
                .tempdir_in(&config.profile)?,
        );
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
        let child = match pair.slave.spawn_command(command) {
            Ok(child) => child,
            Err(error) => {
                #[cfg(windows)]
                retire_unspawned_pair(pair, session);
                return Err(io::Error::other(error));
            }
        };
        #[cfg(windows)]
        let job = {
            let attached = child
                .as_raw_handle()
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))
                .and_then(|handle| unsafe { WindowsJob::attach_raw_handle(handle) });
            match attached {
                Ok(job) => job,
                Err(error) => {
                    // No Job is available yet. Do not wait without a bound or
                    // synchronously drop the last ConPTY owner here.
                    drop(pair.slave);
                    retire_unattached_child(child, pair.master, session);
                    return Err(error);
                }
            }
        };
        drop(pair.slave);
        let output_bytes = Arc::new(AtomicUsize::new(0));
        let voice_status = Arc::new(Mutex::new(VoiceStatusProbe::default()));
        let reader_done = Arc::new(AtomicBool::new(false));
        // The child is now live. Publish its process/PTY owner before any
        // more fallible setup; failed writer/reader setup must still reap it.
        let mut terminal = Self {
            master: Some(pair.master),
            child: Some(child),
            #[cfg(windows)]
            job: Some(job),
            #[cfg(windows)]
            closing: None,
            #[cfg(windows)]
            closed: false,
            writer: None,
            reader: None,
            reader_done: reader_done.clone(),
            output_bytes: output_bytes.clone(),
            voice_status: voice_status.clone(),
            session,
            captures: 0,
        };
        terminal.writer = match terminal.master.as_ref().unwrap().take_writer() {
            Ok(writer) => Some(writer),
            Err(error) => {
                terminal.retire_unpublished();
                return Err(io::Error::other(error));
            }
        };
        let mut output = match terminal.master.as_ref().unwrap().try_clone_reader() {
            Ok(output) => output,
            Err(error) => {
                terminal.retire_unpublished();
                return Err(io::Error::other(error));
            }
        };
        let reader = thread::Builder::new()
            .name("voicetype-google-pty".into())
            .spawn(move || {
                let mut buffer = [0u8; 8192];
                while let Ok(n) = output.read(&mut buffer) {
                    if n == 0 {
                        break;
                    }
                    output_bytes.fetch_add(n, Ordering::AcqRel);
                    if let Ok(mut probe) = voice_status.lock() {
                        probe.feed(&buffer[..n]);
                    }
                }
                reader_done.store(true, Ordering::Release);
            });
        match reader {
            Ok(reader) => {
                terminal.reader = Some(reader);
                Ok(terminal)
            }
            Err(error) => {
                terminal.retire_unpublished();
                Err(error)
            }
        }
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
        let writer = self.writer.as_mut().ok_or(io::ErrorKind::BrokenPipe)?;
        writer.write_all(control.bytes())?;
        writer.flush()
    }

    /// Call immediately before F5. A new generation cannot inherit a prior
    /// status render; only a bounded, volatile status candidate is inspected.
    pub fn arm_voice_status(&mut self) -> io::Result<u64> {
        self.voice_status
            .lock()
            .map(|mut status| status.arm())
            .map_err(|_| io::ErrorKind::Other.into())
    }

    pub fn voice_status_ready(&self, generation: u64) -> io::Result<bool> {
        self.voice_status
            .lock()
            .map(|status| status.ready(generation))
            .map_err(|_| io::ErrorKind::Other.into())
    }

    pub fn fail_voice_status(&mut self) -> io::Result<()> {
        self.voice_status
            .lock()
            .map(|mut status| status.fail())
            .map_err(|_| io::ErrorKind::Other.into())
    }

    pub fn capture_dir(&self) -> PathBuf {
        self.session.path().join("capture")
    }
    pub fn log_path(&self) -> PathBuf {
        self.session.path().join("official-cli.log")
    }

    /// Process state only; terminal output and draft text never enter status.
    pub fn child_exit_observed(&mut self) -> io::Result<bool> {
        Ok(self
            .child
            .as_mut()
            .ok_or(io::ErrorKind::BrokenPipe)?
            .try_wait()?
            .is_some())
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
        #[cfg(windows)]
        if self.closed {
            return Ok(());
        }
        // Close input before the pseudoconsole. Its output clone is drained by
        // the reader thread until ClosePseudoConsole releases the write side.
        self.writer.take();
        if let Some(child) = self.child.as_mut() {
            #[cfg(windows)]
            {
                // portable-pty's WinChild::kill swallows TerminateProcess
                // failures and WinChild::wait waits forever. Keep the Job
                // owner through any failed observation. The caller must keep
                // this terminal and its input lease until cleanup succeeds.
                self.job
                    .as_ref()
                    .ok_or(io::ErrorKind::BrokenPipe)?
                    .terminate()?;
                let until = Instant::now() + Duration::from_secs(2);
                while child.try_wait()?.is_none() {
                    if Instant::now() >= until {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
            #[cfg(unix)]
            {
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
            }
            self.child.take();
        }
        #[cfg(windows)]
        {
            // ClosePseudoConsole may wait for its final output frame on older
            // Windows. The reader remains live in a separate thread, and the
            // close owner retains the session until the kernel call returns.
            // A timeout is not a successful cleanup: the caller must retain
            // its session ownership and poll this completion on retry.
            self.job.take();
            if self.closing.is_none() {
                type CloseParts = (
                    Box<dyn MasterPty + Send>,
                    Option<JoinHandle<()>>,
                    Arc<tempfile::TempDir>,
                );
                let (handoff, incoming) = mpsc::sync_channel::<CloseParts>(1);
                let (complete, completion) = mpsc::channel();
                thread::Builder::new()
                    .name("voicetype-google-conpty-close".into())
                    .spawn(move || {
                        if let Ok((master, reader, session)) = incoming.recv() {
                            drop(master);
                            if let Some(reader) = reader {
                                let _ = reader.join();
                            }
                            drop(session);
                            let _ = complete.send(());
                        }
                    })?;
                let master = self.master.take().ok_or(io::ErrorKind::BrokenPipe)?;
                let reader = self.reader.take();
                let session = self.session.clone();
                if let Err(error) = handoff.send((master, reader, session)) {
                    let (master, reader, _) = error.0;
                    self.master = Some(master);
                    self.reader = reader;
                    return Err(io::ErrorKind::BrokenPipe.into());
                }
                self.closing = Some(completion);
            }
            match self
                .closing
                .as_ref()
                .ok_or(io::ErrorKind::BrokenPipe)?
                .recv_timeout(Duration::from_millis(500))
            {
                Ok(()) => {
                    self.closing.take();
                    self.closed = true;
                    Ok(())
                }
                Err(mpsc::RecvTimeoutError::Timeout) => Err(io::ErrorKind::TimedOut.into()),
                Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::ErrorKind::BrokenPipe.into()),
            }
        }
        #[cfg(unix)]
        {
            self.master.take();
            let until = Instant::now() + Duration::from_millis(500);
            while self.reader.is_some()
                && !self.reader_done.load(Ordering::Acquire)
                && Instant::now() < until
            {
                thread::sleep(Duration::from_millis(5));
            }
            if self.reader.is_some() && !self.reader_done.load(Ordering::Acquire) {
                return Err(io::ErrorKind::TimedOut.into());
            }
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
            Ok(())
        }
    }
}

#[cfg(windows)]
fn retire_unspawned_pair(pair: PtyPair, session: Arc<tempfile::TempDir>) {
    UNPUBLISHED_CLEANUPS.fetch_add(1, Ordering::AcqRel);
    let owner = Arc::new(Mutex::new(Some((pair, session))));
    let retry = owner.clone();
    let spawned = thread::Builder::new()
        .name("voicetype-google-unspawned-pty-close".into())
        .spawn(move || {
            let (pair, session) = retry.lock().unwrap().take().unwrap();
            drop(pair);
            drop(session);
            UNPUBLISHED_CLEANUPS.fetch_sub(1, Ordering::AcqRel);
        });
    if spawned.is_err() {
        std::mem::forget(owner);
    }
}

#[cfg(windows)]
fn retire_unattached_child(
    child: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    session: Arc<tempfile::TempDir>,
) {
    UNPUBLISHED_CLEANUPS.fetch_add(1, Ordering::AcqRel);
    let owner = Arc::new(Mutex::new(Some((child, master, session))));
    let retry = owner.clone();
    let spawned = thread::Builder::new()
        .name("voicetype-google-unattached-cleanup".into())
        .spawn(move || {
            let (mut child, master, session) = retry.lock().unwrap().take().unwrap();
            loop {
                let _ = child.kill();
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            drop(child);
            drop(master);
            drop(session);
            UNPUBLISHED_CLEANUPS.fetch_sub(1, Ordering::AcqRel);
        });
    if spawned.is_err() {
        std::mem::forget(owner);
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
