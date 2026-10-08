//! One CPU worker owned through private pipes. Late replies are drained off the
//! dictation thread; busy, failed and timed-out requests preserve the original.
use crate::spelling_policy::{accepts, apply_reply, MAX_REPLY};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpellingPaths {
    pub executable: std::path::PathBuf,
    pub model: std::path::PathBuf,
    pub tokenizer: std::path::PathBuf,
    pub model_sha256: String,
    pub tokenizer_sha256: String,
    pub threads: u32,
}
impl SpellingPaths {
    pub fn validate(&self) -> io::Result<()> {
        if [&self.executable, &self.model, &self.tokenizer]
            .iter()
            .any(|p| !p.is_absolute())
            || ![1, 2, 4, 8].contains(&self.threads)
            || [&self.model_sha256, &self.tokenizer_sha256]
                .iter()
                .any(|hash| {
                    hash.len() != 64
                        || !hash
                            .bytes()
                            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                })
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(())
    }
    pub fn command(&self) -> io::Result<Command> {
        self.validate()?;
        let mut command = Command::new(&self.executable);
        command
            .env_clear()
            .env("CUDA_VISIBLE_DEVICES", "")
            .env("LANG", "C.UTF-8")
            .arg("--stdio")
            .arg("--model")
            .arg(&self.model)
            .arg("--tokenizer")
            .arg(&self.tokenizer)
            .arg("--model-sha256")
            .arg(&self.model_sha256)
            .arg("--tokenizer-sha256")
            .arg(&self.tokenizer_sha256)
            .arg("--threads")
            .arg(self.threads.to_string());
        // Windows loader/temporary directories only; no ambient Python imports,
        // model flags, credentials or accelerator library search overrides.
        #[cfg(windows)]
        for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        Ok(command)
    }
}

struct Request {
    bytes: Vec<u8>,
    text: String,
    terms: Vec<String>,
    id: u64,
    until: Instant,
    reply: mpsc::SyncSender<io::Result<String>>,
}

pub struct OwnedSpelling {
    child: Mutex<Option<Child>>,
    thread: Option<JoinHandle<()>>,
    sender: Option<mpsc::SyncSender<Request>>,
    busy: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    next_id: AtomicU64,
    #[cfg(windows)]
    job: Option<Job>,
}

impl OwnedSpelling {
    /// Native callers supply a verified executable/catalog. Never expose an
    /// arbitrary Command or executable path through a webview command.
    pub fn start(mut command: Command, budget: Duration) -> io::Result<Self> {
        if budget.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let until = Instant::now() + budget.min(Duration::from_secs(30));
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            let parent = std::process::id() as libc::pid_t;
            // SAFETY: only async-signal-safe syscalls run between fork and exec.
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::getppid() != parent {
                        return Err(io::ErrorKind::Interrupted.into());
                    }
                    Ok(())
                });
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        }
        let child = command.spawn()?;
        let (sender, receiver) = mpsc::sync_channel::<Request>(1);
        let busy = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let mut owner = Self {
            child: Mutex::new(Some(child)),
            thread: None,
            sender: Some(sender),
            busy: busy.clone(),
            alive: alive.clone(),
            next_id: AtomicU64::new(1),
            #[cfg(windows)]
            job: None,
        };
        #[cfg(windows)]
        {
            let child = owner.child.lock().map_err(|_| io::ErrorKind::Other)?;
            owner.job = Some(Job::attach(child.as_ref().unwrap())?);
        }
        let mut child_guard = owner.child.lock().map_err(|_| io::ErrorKind::Other)?;
        let child = child_guard.as_mut().unwrap();
        let mut input = child.stdin.take().ok_or(io::ErrorKind::BrokenPipe)?;
        let mut output = BufReader::new(child.stdout.take().ok_or(io::ErrorKind::BrokenPipe)?);
        drop(child_guard);
        let (ready, readiness) = mpsc::sync_channel(1);
        owner.thread = Some(
            thread::Builder::new()
                .name("voicetype-spelling-pipe".into())
                .spawn(move || {
                    let negotiated = frame(&mut output).and_then(|bytes| {
                        let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
                        if value
                            != json!({"v":1,"status":"ready","provider":"CPUExecutionProvider"})
                        {
                            return Err(invalid());
                        }
                        Ok(())
                    });
                    let success = negotiated.is_ok();
                    let _ = ready.send(negotiated);
                    if success {
                        while let Ok(request) = receiver.recv() {
                            if Instant::now() >= request.until {
                                let _ = request.reply.send(Err(io::ErrorKind::TimedOut.into()));
                                busy.store(false, Ordering::Release);
                                continue;
                            }
                            let result = input
                                .write_all(&request.bytes)
                                .and_then(|_| input.flush())
                                .and_then(|_| frame(&mut output))
                                .and_then(|bytes| {
                                    let terms: Vec<&str> =
                                        request.terms.iter().map(String::as_str).collect();
                                    apply_reply(&request.text, &terms, request.id, &bytes)
                                        .map_err(|_| invalid())
                                });
                            let failed = result.is_err();
                            busy.store(false, Ordering::Release);
                            let _ = request.reply.send(result);
                            if failed {
                                break;
                            }
                        }
                    }
                    alive.store(false, Ordering::Release);
                })?,
        );
        let remaining = until.saturating_duration_since(Instant::now());
        readiness
            .recv_timeout(remaining)
            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
        Ok(owner)
    }

    pub fn correct(&self, text: &str, terms: &[String]) -> String {
        let original = || text.to_owned();
        if !accepts(text) || !self.healthy() {
            return original();
        }
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return original();
        }
        let until = Instant::now() + Duration::from_millis(100);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let terms: Vec<&str> = terms
            .iter()
            .map(String::as_str)
            .filter(|t| !t.is_empty() && t.chars().count() <= 64 && text.contains(t))
            .take(256)
            .collect();
        let sent = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let mut bytes =
            serde_json::to_vec(&json!({"v":1,"id":id,"text":text,"terms":terms,"sent_at_ms":sent}))
                .unwrap();
        bytes.push(b'\n');
        let (reply, response) = mpsc::sync_channel(1);
        let request = Request {
            bytes,
            text: text.to_owned(),
            terms: terms.iter().map(|term| (*term).to_owned()).collect(),
            id,
            until,
            reply,
        };
        if self
            .sender
            .as_ref()
            .is_none_or(|s| s.try_send(request).is_err())
        {
            self.busy.store(false, Ordering::Release);
            return original();
        }
        let result = response.recv_timeout(until.saturating_duration_since(Instant::now()));
        if let Ok(Ok(output)) = result {
            if Instant::now() < until {
                return output;
            }
        }
        original()
    }

    pub fn process_id(&self) -> Option<u32> {
        self.child.lock().ok()?.as_ref().map(Child::id)
    }

    pub fn is_running(&self) -> io::Result<bool> {
        if !self.alive.load(Ordering::Acquire) {
            return Ok(false);
        }
        let mut child = self
            .child
            .lock()
            .map_err(|_| io::Error::other("spelling owner failed"))?;
        let running = match child.as_mut() {
            Some(child) => child.try_wait()?.is_none(),
            None => false,
        };
        if !running {
            self.alive.store(false, Ordering::Release);
        }
        Ok(running)
    }

    pub fn healthy(&self) -> bool {
        self.is_running().unwrap_or(false)
    }

    pub fn shutdown(&mut self) -> io::Result<()> {
        self.alive.store(false, Ordering::Release);
        self.sender.take();
        let mut child = self
            .child
            .lock()
            .map_err(|_| io::Error::other("spelling owner failed"))?;
        if let Some(child) = child.as_mut() {
            if child.try_wait()?.is_none() {
                child.kill()?;
            }
            child.wait()?;
        }
        *child = None;
        drop(child);
        if let Some(worker) = self.thread.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("spelling pipe failed"))?;
        }
        #[cfg(windows)]
        {
            self.job.take();
        }
        Ok(())
    }
}
impl Drop for OwnedSpelling {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid spelling frame")
}
fn frame(reader: &mut impl BufRead) -> io::Result<Vec<u8>> {
    let mut result = Vec::new();
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let end = buffer.iter().position(|b| *b == b'\n');
        let count = end.map_or(buffer.len(), |end| end + 1);
        if result.len() + count > MAX_REPLY {
            return Err(invalid());
        }
        result.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        if end.is_some() {
            result.pop();
            return Ok(result);
        }
    }
}

#[cfg(windows)]
struct Job(windows_sys::Win32::Foundation::HANDLE);
// SAFETY: the handle is private and immutable; close requires exclusive Drop.
#[cfg(windows)]
unsafe impl Send for Job {}
#[cfg(windows)]
unsafe impl Sync for Job {}
#[cfg(windows)]
impl Job {
    fn attach(child: &Child) -> io::Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::*;
        // SAFETY: owned valid handles and correctly sized job information; the
        // kernel closes all contained processes when this unshared handle dies.
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Self(handle);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            ) == 0
                || AssignProcessToJobObject(handle, child.as_raw_handle()) == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }
    }
}
#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: the job owns this handle exclusively.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
