//! Linux PipeWire capture and one-consumer PCM relay for the official CLI.
//! Catch-up defaults off: queued PCM is paced at 16 kHz s16 mono.
use super::adapter::GoogleAudio;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::{
        fs::{symlink, PermissionsExt},
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MAX_PCM: u64 = 16_000 * 2 * 60;
const REVIEWED_PIPEWIRE_105: &str =
    "3874ae059c9eafdbd4d6fde4d4b7553aa83fe4149246e427a3208bdc4c712c64";

pub struct LinuxPcmAudio {
    root: tempfile::TempDir,
    native_recorder: PathBuf,
    allow_sigint_exit_one: bool,
    listener: Option<UnixListener>,
    nonce: String,
    child: Option<Child>,
    reader: Option<JoinHandle<io::Result<u64>>>,
    relay: Option<JoinHandle<io::Result<()>>>,
    connected: Arc<AtomicBool>,
    drained: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    stop_requested: Arc<AtomicBool>,
    total_sent: Arc<AtomicU64>,
    total_produced: Arc<AtomicU64>,
    catchup: bool,
}

impl LinuxPcmAudio {
    /// The caller supplies a reviewed native PipeWire hash and the bundled
    /// relay helper; neither comes from web content or PATH.
    pub fn new(
        profile: &Path,
        native_recorder: &Path,
        native_sha256: &str,
        shim: &Path,
    ) -> io::Result<Self> {
        verify_executable(native_recorder, native_sha256)?;
        if !shim.is_absolute() || !fs::symlink_metadata(shim)?.file_type().is_file() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let root = tempfile::Builder::new()
            .prefix("pcm-")
            .tempdir_in(profile)?;
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
        let bin = root.path().join("bin");
        fs::create_dir(&bin)?;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o700))?;
        symlink(shim, bin.join("pw-record"))?;
        let socket = root.path().join("relay.sock");
        if socket.as_os_str().len() >= 108 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let listener = UnixListener::bind(&socket)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        let mut random = [0u8; 32];
        File::open("/dev/urandom")?.read_exact(&mut random)?;
        let nonce = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let allow_sigint_exit_one = native_recorder == Path::new("/usr/bin/pw-record")
            && native_sha256.eq_ignore_ascii_case(REVIEWED_PIPEWIRE_105);
        Ok(Self {
            root,
            native_recorder: native_recorder.into(),
            allow_sigint_exit_one,
            listener: Some(listener),
            nonce,
            child: None,
            reader: None,
            relay: None,
            connected: Arc::new(AtomicBool::new(false)),
            drained: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            closing: Arc::new(AtomicBool::new(false)),
            stop_requested: Arc::new(AtomicBool::new(false)),
            total_sent: Arc::new(AtomicU64::new(0)),
            total_produced: Arc::new(AtomicU64::new(0)),
            catchup: false,
        })
    }

    /// Explicit opt-in for a faster stopped tail. Normal pacing is default.
    pub fn with_catchup(mut self, enabled: bool) -> Self {
        self.catchup = enabled;
        self
    }

    pub fn catchup_enabled(&self) -> bool {
        self.catchup
    }

    fn wait_flag(&self, flag: &AtomicBool, limit: Duration) -> io::Result<()> {
        let until = Instant::now() + limit;
        while !flag.load(Ordering::Acquire) {
            if self.cancelled.load(Ordering::Acquire) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if self.relay.as_ref().is_some_and(JoinHandle::is_finished) {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            if Instant::now() >= until {
                return Err(io::ErrorKind::TimedOut.into());
            }
            thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
}

impl GoogleAudio for LinuxPcmAudio {
    fn terminal_environment(&self) -> Vec<(OsString, OsString)> {
        vec![
            (
                "PATH".into(),
                format!("{}:/usr/bin:/bin", self.root.path().join("bin").display()).into(),
            ),
            (
                "VOICETYPE_GOOGLE_RELAY".into(),
                self.root.path().join("relay.sock").into_os_string(),
            ),
            ("VOICETYPE_GOOGLE_NONCE".into(), self.nonce.clone().into()),
        ]
    }

    fn set_cancellation(&mut self, flag: Arc<AtomicBool>) {
        self.cancelled = flag;
    }

    fn start(&mut self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if self.child.is_some() || self.relay.is_some() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        let listener = self.listener.take().ok_or(io::ErrorKind::BrokenPipe)?;
        let (sender, receiver) = mpsc::sync_channel(128);
        let connected = self.connected.clone();
        let drained = self.drained.clone();
        let cancelled = self.cancelled.clone();
        let closing = self.closing.clone();
        let stop_requested = self.stop_requested.clone();
        let sent = self.total_sent.clone();
        let produced = self.total_produced.clone();
        let nonce = self.nonce.clone();
        let catchup = self.catchup;
        self.relay = Some(
            thread::Builder::new()
                .name("voicetype-google-pcm-relay".into())
                .spawn(move || {
                    serve(
                        listener,
                        nonce,
                        receiver,
                        connected,
                        drained,
                        cancelled,
                        closing,
                        stop_requested,
                        sent,
                        produced.clone(),
                        catchup,
                    )
                })?,
        );
        let mut command = Command::new(&self.native_recorder);
        command
            .args(["--rate=16000", "--channels=1", "--format=s16", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let mut child = command.spawn()?;
        let output = child.stdout.take().ok_or(io::ErrorKind::BrokenPipe)?;
        self.child = Some(child);
        let closing = self.closing.clone();
        let produced = self.total_produced.clone();
        self.reader = Some(
            thread::Builder::new()
                .name("voicetype-google-pcm-read".into())
                .spawn(move || read_pcm(output, sender, closing, produced))?,
        );
        Ok(())
    }

    fn wait_connected(&mut self) -> io::Result<()> {
        self.wait_flag(&self.connected, Duration::from_secs(12))
    }

    fn stop_and_drain(&mut self) -> io::Result<()> {
        self.stop_requested.store(true, Ordering::Release);
        let child = self.child.as_mut().ok_or(io::ErrorKind::BrokenPipe)?;
        unsafe {
            libc::kill(
                -i32::try_from(child.id()).map_err(|_| io::ErrorKind::InvalidData)?,
                libc::SIGINT,
            );
        }
        let until = Instant::now() + Duration::from_secs(1);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= until {
                return Err(io::ErrorKind::TimedOut.into());
            }
            thread::sleep(Duration::from_millis(5));
        };
        let bytes = self
            .reader
            .take()
            .ok_or(io::ErrorKind::BrokenPipe)?
            .join()
            .map_err(|_| io::ErrorKind::Other)??;
        if bytes == 0
            || bytes % 2 != 0
            || bytes > MAX_PCM
            || !(status.success() || (self.allow_sigint_exit_one && status.code() == Some(1)))
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.wait_flag(&self.drained, Duration::from_secs(18))?;
        if self.total_sent.load(Ordering::Acquire) != bytes {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(())
    }

    fn wait_recorder_exit(&mut self) -> io::Result<()> {
        let until = Instant::now() + Duration::from_secs(1);
        while !self.relay.as_ref().is_some_and(JoinHandle::is_finished) {
            if Instant::now() >= until {
                return Err(io::ErrorKind::TimedOut.into());
            }
            thread::sleep(Duration::from_millis(5));
        }
        self.relay
            .take()
            .ok_or(io::ErrorKind::BrokenPipe)?
            .join()
            .map_err(|_| io::ErrorKind::Other)??;
        Ok(())
    }

    fn cleanup(&mut self) -> io::Result<()> {
        self.closing.store(true, Ordering::Release);
        if let Some(child) = self.child.as_mut() {
            if child.try_wait()?.is_none() {
                unsafe {
                    libc::kill(
                        -i32::try_from(child.id()).map_err(|_| io::ErrorKind::InvalidData)?,
                        libc::SIGKILL,
                    );
                }
            }
            child.wait()?;
            self.child.take();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some(relay) = self.relay.as_ref() {
            let until = Instant::now() + Duration::from_secs(1);
            while !relay.is_finished() {
                if Instant::now() >= until {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
        if let Some(relay) = self.relay.take() {
            let _ = relay.join();
        }
        Ok(())
    }
}

impl Drop for LinuxPcmAudio {
    fn drop(&mut self) {
        // Last-resort child ownership for direct users and partially failed
        // startup. The provider normally keeps this object until cleanup()
        // succeeds; dropping Child alone would leave its process running.
        self.closing.store(true, Ordering::Release);
        self.cancelled.store(true, Ordering::Release);
        if let Some(mut child) = self.child.take() {
            if !matches!(child.try_wait(), Ok(Some(_))) {
                if let Ok(pid) = i32::try_from(child.id()) {
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                }
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some(relay) = self.relay.take() {
            let _ = relay.join();
        }
    }
}

fn verify_executable(path: &Path, expected: &str) -> io::Result<()> {
    if !path.is_absolute()
        || expected.len() != 64
        || !expected.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file()
        && !(path == Path::new("/usr/bin/pw-record")
            && meta.file_type().is_symlink()
            && fs::canonicalize(path)? == Path::new("/usr/bin/pw-cat"))
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    io::copy(&mut file, &mut hash)?;
    if format!("{:x}", hash.finalize()) != expected.to_ascii_lowercase() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(())
}

fn read_pcm(
    mut output: impl Read,
    sender: SyncSender<Vec<u8>>,
    closing: Arc<AtomicBool>,
    produced: Arc<AtomicU64>,
) -> io::Result<u64> {
    let mut total = 0u64;
    let mut held = Vec::<u8>::new();
    let mut block = [0u8; 4096];
    loop {
        let n = output.read(&mut block)?;
        if n == 0 {
            break;
        }
        held.extend_from_slice(&block[..n]);
        while held.len() >= 640 {
            let frame = held.drain(..640).collect::<Vec<_>>();
            total += 640;
            if total > MAX_PCM {
                return Err(io::ErrorKind::InvalidData.into());
            }
            produced.store(total, Ordering::Release);
            send_frame(&sender, frame, &closing)?;
        }
    }
    if held.len() % 2 != 0 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    if !held.is_empty() {
        total += held.len() as u64;
        if total > MAX_PCM {
            return Err(io::ErrorKind::InvalidData.into());
        }
        produced.store(total, Ordering::Release);
        send_frame(&sender, held, &closing)?;
    }
    Ok(total)
}

fn send_frame(
    sender: &SyncSender<Vec<u8>>,
    mut frame: Vec<u8>,
    closing: &AtomicBool,
) -> io::Result<()> {
    loop {
        match sender.try_send(frame) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Disconnected(_)) => return Err(io::ErrorKind::BrokenPipe.into()),
            Err(TrySendError::Full(returned)) => {
                if closing.load(Ordering::Acquire) {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                frame = returned;
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

fn serve(
    listener: UnixListener,
    nonce: String,
    receiver: Receiver<Vec<u8>>,
    connected: Arc<AtomicBool>,
    drained: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    stop_requested: Arc<AtomicBool>,
    sent: Arc<AtomicU64>,
    produced: Arc<AtomicU64>,
    catchup: bool,
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    let until = Instant::now() + Duration::from_secs(12);
    let mut client = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if closing.load(Ordering::Acquire) || cancelled.load(Ordering::Acquire) {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                if Instant::now() >= until {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    };
    client.set_read_timeout(Some(Duration::from_millis(100)))?;
    client.set_write_timeout(Some(Duration::from_millis(100)))?;
    let mut authorization = [0u8; 64];
    read_exact_checked(&mut client, &mut authorization, &closing)?;
    if authorization != nonce.as_bytes() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    client.write_all(b"OK")?;
    connected.store(true, Ordering::Release);
    let mut pacer = PcmPacer::new();
    let mut byte_count = 0u64;
    loop {
        let mut request = [0u8; 4];
        read_exact_checked(&mut client, &mut request, &closing)?;
        if u32::from_be_bytes(request) != 640 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let frame = loop {
            match receiver.recv_timeout(Duration::from_millis(20)) {
                Ok(frame) => break Some(frame),
                Err(mpsc::RecvTimeoutError::Disconnected) => break None,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if closing.load(Ordering::Acquire) || cancelled.load(Ordering::Acquire) {
                        return Err(io::ErrorKind::Interrupted.into());
                    }
                }
            }
        };
        let Some(frame) = frame else {
            client.write_all(&0u32.to_be_bytes())?;
            let mut ack = [0u8; 4];
            read_exact_checked(&mut client, &mut ack, &closing)?;
            if ack != 0u32.to_be_bytes() {
                return Err(io::ErrorKind::InvalidData.into());
            }
            drained.store(true, Ordering::Release);
            // Wait for the official CLI to close its recorder pipe after F5.
            let until = Instant::now() + Duration::from_secs(2);
            let mut probe = [0u8; 1];
            loop {
                match client.read(&mut probe) {
                    Ok(0) => return Ok(()),
                    Ok(_) => return Err(io::ErrorKind::InvalidData.into()),
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            || error.kind() == io::ErrorKind::TimedOut =>
                    {
                        if closing.load(Ordering::Acquire) {
                            return Err(io::ErrorKind::Interrupted.into());
                        }
                        if Instant::now() >= until {
                            return Err(io::ErrorKind::TimedOut.into());
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
        };
        if frame.len() > 640 || frame.len() % 2 != 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        client.write_all(&(frame.len() as u32).to_be_bytes())?;
        client.write_all(&frame)?;
        let mut ack = [0u8; 4];
        read_exact_checked(&mut client, &mut ack, &closing)?;
        if u32::from_be_bytes(ack) != frame.len() as u32 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        byte_count += frame.len() as u64;
        sent.store(byte_count, Ordering::Release);
        let backlog = produced.load(Ordering::Acquire).saturating_sub(byte_count);
        let target = pacer.after_frame(
            Instant::now(),
            frame.len(),
            backlog,
            catchup && stop_requested.load(Ordering::Acquire),
        );
        while Instant::now() < target {
            if closing.load(Ordering::Acquire) || cancelled.load(Ordering::Acquire) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            thread::sleep(
                target
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(5)),
            );
        }
    }
}

struct PcmPacer {
    deadline: Instant,
    catching_up: bool,
}

impl PcmPacer {
    fn new() -> Self {
        Self {
            deadline: Instant::now(),
            catching_up: false,
        }
    }

    fn after_frame(
        &mut self,
        now: Instant,
        bytes: usize,
        backlog: u64,
        allow_catchup: bool,
    ) -> Instant {
        // 200 ms to enter and below 100 ms to leave; catch-up only after
        // explicit stop, and never faster than twice the native sample rate.
        if !allow_catchup || (self.catching_up && backlog < 3_200) {
            self.catching_up = false;
        } else if backlog >= 6_400 {
            self.catching_up = true;
        }
        let speed = if self.catching_up { 2.0 } else { 1.0 };
        self.deadline =
            self.deadline.max(now) + Duration::from_secs_f64(bytes as f64 / 32_000.0 / speed);
        self.deadline
    }
}

fn read_exact_checked(
    stream: &mut UnixStream,
    mut output: &mut [u8],
    closing: &AtomicBool,
) -> io::Result<()> {
    while !output.is_empty() {
        match stream.read(output) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => output = &mut output[n..],
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::TimedOut =>
            {
                if closing.load(Ordering::Acquire) {
                    return Err(io::ErrorKind::Interrupted.into());
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_relay_does_not_burst_queued_pcm_after_consumer_stall() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("relay.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (sender, receiver) = mpsc::sync_channel(32);
        for _ in 0..12 {
            sender.send(vec![1u8; 640]).unwrap();
        }
        drop(sender);
        let connected = Arc::new(AtomicBool::new(false));
        let drained = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let closing = Arc::new(AtomicBool::new(false));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(AtomicU64::new(0));
        let produced = Arc::new(AtomicU64::new(12 * 640));
        let server = thread::spawn({
            let connected = connected.clone();
            let drained = drained.clone();
            let cancelled = cancelled.clone();
            let closing = closing.clone();
            let stop_requested = stop_requested.clone();
            let sent = sent.clone();
            let produced = produced.clone();
            move || {
                serve(
                    listener,
                    "a".repeat(64),
                    receiver,
                    connected,
                    drained,
                    cancelled,
                    closing,
                    stop_requested,
                    sent,
                    produced,
                    false,
                )
            }
        });
        let mut client = UnixStream::connect(socket).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client.write_all("a".repeat(64).as_bytes()).unwrap();
        let mut hello = [0; 2];
        client.read_exact(&mut hello).unwrap();
        assert_eq!(&hello, b"OK");
        let mut pull = || {
            client.write_all(&640u32.to_be_bytes()).unwrap();
            let mut len = [0; 4];
            client.read_exact(&mut len).unwrap();
            let n = u32::from_be_bytes(len) as usize;
            let mut frame = vec![0; n];
            client.read_exact(&mut frame).unwrap();
            client.write_all(&(n as u32).to_be_bytes()).unwrap();
            n
        };
        assert_eq!(pull(), 640);
        thread::sleep(Duration::from_millis(350));
        let resumed = Instant::now();
        for _ in 0..8 {
            assert_eq!(pull(), 640);
        }
        assert!(
            resumed.elapsed() >= Duration::from_millis(130),
            "queued frames burst faster than the normal 1x PCM rate"
        );
        for _ in 0..3 {
            assert_eq!(pull(), 640);
        }
        assert_eq!(pull(), 0);
        drop(pull);
        drop(client);
        server.join().unwrap().unwrap();
        assert!(drained.load(Ordering::Acquire));
        assert_eq!(sent.load(Ordering::Acquire), 12 * 640);
    }

    #[test]
    fn optional_catchup_uses_200ms_enter_100ms_exit_and_never_exceeds_2x() {
        let mut pacer = PcmPacer::new();
        let now = Instant::now();
        let one = pacer.after_frame(now, 640, 6_400, false);
        assert_eq!(one.duration_since(now), Duration::from_millis(20));
        let two = pacer.after_frame(one, 640, 6_399, true);
        assert_eq!(two.duration_since(one), Duration::from_millis(20));
        let three = pacer.after_frame(two, 640, 6_400, true);
        assert_eq!(three.duration_since(two), Duration::from_millis(10));
        let four = pacer.after_frame(three, 640, 3_200, true);
        assert_eq!(four.duration_since(three), Duration::from_millis(10));
        let five = pacer.after_frame(four, 640, 3_199, true);
        assert_eq!(five.duration_since(four), Duration::from_millis(20));
        let stalled = pacer.after_frame(five + Duration::from_secs(1), 640, 20_000, true);
        assert_eq!(
            stalled.duration_since(five + Duration::from_secs(1)),
            Duration::from_millis(10)
        );
    }
}
