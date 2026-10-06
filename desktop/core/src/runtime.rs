//! Ownership of a private Linux Nano CPU process. Preparing this runtime never
//! opens capture or touches an installed daily service. Asset installation and
//! input-integration migration are separate application operations.
use crate::{dispatch::LocalDispatcher, local::LocalConnection, Application};
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt},
        net::UnixStream,
        process::CommandExt,
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub struct LocalRuntimePaths {
    pub executable: PathBuf,
    pub model_dir: PathBuf,
    pub vad_model: PathBuf,
    pub profile: PathBuf,
}

pub struct OwnedLocal {
    child: Option<Child>,
    connection: Option<LocalConnection>,
    endpoint_dir: Option<tempfile::TempDir>,
    // Retain the profile lock through cleanup. A second owner must not open
    // another model/microphone or write the same personal-data profile.
    profile_lock: Option<File>,
    // Linux parent-death delivery follows the spawning thread. Keep this owner
    // on its long-lived native worker, never a transient UI/blocking-pool task.
    _worker_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl OwnedLocal {
    pub fn start(paths: &LocalRuntimePaths, budget: Duration) -> io::Result<Self> {
        if budget.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        if [
            &paths.executable,
            &paths.model_dir,
            &paths.vad_model,
            &paths.profile,
        ]
        .iter()
        .any(|path| !path.is_absolute())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "runtime paths must be absolute",
            ));
        }
        if !paths.executable.is_file() || !paths.model_dir.is_dir() || !paths.vad_model.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "local runtime or models are missing",
            ));
        }
        let until = Instant::now() + budget.min(Duration::from_secs(30));
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&paths.profile)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(paths.profile.join("runtime.lock"))?;
        lock.try_lock().map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "local profile already has an owner",
            )
        })?;
        for name in ["home", "config", "data"] {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(paths.profile.join(name))?;
        }
        // Keep Unix socket paths short even when the user's profile path is long.
        // tempfile creates this exclusive directory with mode 0700.
        let endpoint_dir = tempfile::Builder::new()
            .prefix("voicetype-app-")
            .tempdir_in("/tmp")?;
        let endpoint = endpoint_dir.path().join("engine.sock");
        let mut command = Command::new(&paths.executable);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("HOME", paths.profile.join("home"))
            .env("XDG_CONFIG_HOME", paths.profile.join("config"))
            .env("XDG_DATA_HOME", paths.profile.join("data"))
            .env("CUDA_VISIBLE_DEVICES", "")
            .env("VOICETYPE_ASR_PROFILE", "nano")
            .env("VOICETYPE_NANO_MODEL_DIR", &paths.model_dir)
            .env("VOICETYPE_NANO_VAD_MODEL", &paths.vad_model)
            .env("VOICETYPE_SOCKET", &endpoint)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Explicit OS audio/notice access only. Do not inherit provider flags,
        // model overrides, screen helpers, credentials or arbitrary library paths.
        for name in [
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
            "PULSE_SERVER",
            "PIPEWIRE_REMOTE",
            "PIPEWIRE_RUNTIME_DIR",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let owner_pid = std::process::id() as libc::pid_t;
        // SAFETY: the child-side closure uses only async-signal-safe syscalls and
        // primitive values between fork and exec. SIGKILL also closes capture if
        // the owning application exits without running Rust destructors.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != owner_pid {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        let mut runtime = Self {
            child: Some(child),
            connection: None,
            endpoint_dir: Some(endpoint_dir),
            profile_lock: Some(lock),
            _worker_thread: Default::default(),
        };
        loop {
            let child = runtime.child.as_mut().expect("runtime owns startup child");
            if child.try_wait()?.is_some() {
                return Err(io::Error::other("local engine exited before readiness"));
            }
            let remaining = until
                .checked_duration_since(Instant::now())
                .filter(|value| !value.is_zero())
                .ok_or(io::ErrorKind::TimedOut)?;
            match LocalConnection::connect_to_process(
                &endpoint,
                child.id(),
                remaining.min(Duration::from_millis(300)),
            ) {
                Ok(connection) => {
                    runtime.connection = Some(connection);
                    return Ok(runtime);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    thread::sleep(remaining.min(Duration::from_millis(20)));
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub fn attach_frontend(
        mut self,
        peer: UnixStream,
        budget: Duration,
    ) -> io::Result<OwnedLocalSession> {
        let connection = self.connection.take().ok_or(io::ErrorKind::NotConnected)?;
        let dispatcher = LocalDispatcher::accept(peer, connection, budget)?;
        Ok(OwnedLocalSession {
            runtime: self,
            dispatcher,
        })
    }

    pub fn process_id(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// Success is based on wait/reap, not on sending a stop signal or losing IPC.
    /// The session owner must invalidate pending results before calling this.
    pub fn shutdown(&mut self) -> io::Result<()> {
        self.connection.take();
        if let Some(child) = self.child.as_mut() {
            if child.try_wait()?.is_none() {
                child.kill()?;
            }
            child.wait()?;
            self.child = None;
        }
        if let Some(directory) = self.endpoint_dir.take() {
            directory.close()?;
        }
        self.profile_lock.take();
        Ok(())
    }
}

impl Drop for OwnedLocal {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// A routed local session and its actual child belong to the same native worker.
/// Transport failure first invalidates text, then stops/reaps the child, and only
/// then releases app ownership. Failed cleanup keeps the application busy.
pub struct OwnedLocalSession {
    runtime: OwnedLocal,
    dispatcher: LocalDispatcher,
}

impl OwnedLocalSession {
    pub fn step(&mut self, app: &mut Application, budget: Duration) -> io::Result<()> {
        if self.runtime.process_id().is_none() {
            return Err(io::ErrorKind::NotConnected.into());
        }
        match self.dispatcher.step(app, budget) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.runtime.shutdown()?;
                self.dispatcher.release_after_exit(app);
                Err(error)
            }
        }
    }

    pub fn shutdown(&mut self, app: &mut Application) -> io::Result<()> {
        self.dispatcher.cancel(app);
        self.runtime.shutdown()?;
        self.dispatcher.release_after_exit(app);
        Ok(())
    }

    pub fn process_id(&self) -> Option<u32> {
        self.runtime.process_id()
    }
}
