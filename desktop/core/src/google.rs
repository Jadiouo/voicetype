//! Official interactive CLI voice attempt. OS process, recorder and editor
//! operations enter only through `GoogleBoundary`; no command accepts a prompt
//! or an Enter key. This module does not infer a transcript from terminal bytes.
pub mod adapter;
#[cfg(target_os = "linux")]
pub mod linux_audio;
pub mod release;
pub mod terminal;
use crate::{
    CommandRejected, Provider, ProviderCommand, ProviderEvent, ProviderPort, SessionFailure,
    SessionKey,
};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoogleInstallState {
    Missing,
    Incompatible,
    /// The exact reviewed binary exists. Login, workspace trust and editor
    /// access remain unknown until the official interactive preflight passes.
    OfficialCheckRequired,
}

pub struct GoogleSetup {
    executable: PathBuf,
    reviewed_sha256: String,
}

impl GoogleSetup {
    pub fn new(executable: PathBuf, reviewed_sha256: String) -> Self {
        Self {
            executable,
            reviewed_sha256,
        }
    }

    /// Read-only inspection; selecting Google never logs in or opens a mic.
    pub fn inspect(&self) -> GoogleInstallState {
        let Ok(meta) = fs::symlink_metadata(&self.executable) else {
            return GoogleInstallState::Missing;
        };
        if !self.executable.is_absolute()
            || !meta.file_type().is_file()
            || self.reviewed_sha256.len() != 64
        {
            return GoogleInstallState::Incompatible;
        }
        let Ok(bytes) = fs::read(&self.executable) else {
            return GoogleInstallState::Incompatible;
        };
        if format!("{:x}", Sha256::digest(&bytes)) != self.reviewed_sha256.to_ascii_lowercase() {
            return GoogleInstallState::Incompatible;
        }
        GoogleInstallState::OfficialCheckRequired
    }
}

/// Same OpenCC and explicit vocabulary files used by the Local output path.
/// The optional bounded CPU spelling function is supplied by the app's owner,
/// so Google never inherits a socket, model path or webview command.
pub struct GoogleText {
    traditional: voicetype_text::Traditional,
    vocabulary: voicetype_text::vocab::Vocab,
}

impl GoogleText {
    pub fn open(vocabulary: &Path) -> io::Result<Self> {
        let traditional = voicetype_text::Traditional::load()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "OpenCC s2tw unavailable"))?;
        let vocabulary = voicetype_text::vocab::Vocab::load_or_empty(vocabulary);
        Ok(Self {
            traditional,
            vocabulary,
        })
    }

    pub fn apply(
        &self,
        raw: &str,
        spelling: impl FnOnce(&str, &[String]) -> String,
    ) -> io::Result<String> {
        if raw.chars().count() > 4096 || raw.contains('\0') {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let snapshot = self.vocabulary.snapshot();
        let converted = self
            .traditional
            .convert_preserving(raw, snapshot.names())
            .map_err(io::Error::other)?;
        let (corrected, terms) = snapshot.apply_with_terms(&converted);
        let spelled = spelling(&corrected, &terms);
        let result = self
            .traditional
            .convert_preserving(&spelled, snapshot.names())
            .map_err(io::Error::other)?;
        if result.trim().is_empty()
            || result.len() > 64 * 1024
            || result.contains(['\r', '\n', '\t'])
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(result)
    }
}

/// External CLI/audio boundary. A production implementation must own both the
/// CLI and recorder until `cleanup` confirms that neither can use the microphone.
/// `stop_capture_and_drain` includes recorder stdout EOF and complete relay
/// consumption. `capture_editor` reads an owned draft, never terminal output.
pub trait GoogleBoundary {
    /// Called before any capture so blocking operations can observe cancellation.
    fn set_cancellation(&mut self, _flag: Arc<AtomicBool>) {}
    fn start_capture(&mut self) -> io::Result<()>;
    fn stop_capture_and_drain(&mut self) -> io::Result<()>;
    fn stop_official_voice(&mut self) -> io::Result<()>;
    fn wait_official_recorder(&mut self) -> io::Result<()>;
    fn capture_editor(&mut self) -> io::Result<String>;
    fn settle(&mut self, duration: Duration) -> io::Result<()>;
    fn reject_known_voice_errors(&mut self) -> io::Result<()>;
    fn cleanup(&mut self) -> io::Result<()>;
}

pub struct GoogleAttempt {
    finished: bool,
}

impl GoogleAttempt {
    pub fn start(boundary: &mut impl GoogleBoundary) -> io::Result<Self> {
        if let Err(error) = boundary.start_capture() {
            let _ = boundary.cleanup();
            return Err(error);
        }
        Ok(Self { finished: false })
    }

    /// The second editor capture is the candidate. A stable first capture is
    /// not a reason to skip the second one. Every failed step still cleans up.
    pub fn finish(&mut self, boundary: &mut impl GoogleBoundary) -> io::Result<String> {
        self.finish_with_cancel(boundary, &AtomicBool::new(false))
    }

    fn finish_with_cancel(
        &mut self,
        boundary: &mut impl GoogleBoundary,
        cancelled: &AtomicBool,
    ) -> io::Result<String> {
        if self.finished {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "voice attempt already finished",
            ));
        }
        self.finished = true;
        let result = (|| {
            check_cancelled(cancelled)?;
            boundary.stop_capture_and_drain()?;
            check_cancelled(cancelled)?;
            boundary.stop_official_voice()?;
            check_cancelled(cancelled)?;
            boundary.wait_official_recorder()?;
            check_cancelled(cancelled)?;
            let _first = boundary.capture_editor()?;
            check_cancelled(cancelled)?;
            boundary.settle(Duration::from_millis(200))?;
            check_cancelled(cancelled)?;
            let second = boundary.capture_editor()?;
            check_cancelled(cancelled)?;
            if second.trim().is_empty() || second.len() > 64 * 1024 || second.contains('\0') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid voice draft",
                ));
            }
            Ok(second)
        })();
        let cleanup = boundary.cleanup();
        cleanup?;
        check_cancelled(cancelled)?;
        let text = result?;
        boundary.reject_known_voice_errors()?;
        Ok(text)
    }

    pub fn cancel(&mut self, boundary: &mut impl GoogleBoundary) -> io::Result<()> {
        self.finished = true;
        boundary.cleanup()
    }
}

fn check_cancelled(flag: &AtomicBool) -> io::Result<()> {
    if flag.load(Ordering::Acquire) {
        Err(io::ErrorKind::Interrupted.into())
    } else {
        Ok(())
    }
}

enum Request {
    Start(SessionKey),
    Stop(SessionKey),
    Cancel(SessionKey),
    Shutdown,
}

/// Resident owner for one official CLI and microphone. It maps the bounded
/// external operation to the shared coordinator events. The OS boundary owns
/// actual recorder/PTY process handles; `Released` follows verified cleanup.
pub struct GoogleProvider {
    requests: mpsc::SyncSender<Request>,
    events: mpsc::Receiver<(SessionKey, ProviderEvent)>,
    active: Option<SessionKey>,
    cancelled: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl GoogleProvider {
    pub fn spawn(boundary: impl GoogleBoundary + Send + 'static) -> io::Result<Self> {
        Self::spawn_with_text(boundary, Ok)
    }

    pub fn spawn_with_text(
        mut boundary: impl GoogleBoundary + Send + 'static,
        format: impl Fn(String) -> io::Result<String> + Send + 'static,
    ) -> io::Result<Self> {
        let (requests, input) = mpsc::sync_channel(4);
        let (output, events) = mpsc::sync_channel(4);
        let cancelled = Arc::new(AtomicBool::new(false));
        boundary.set_cancellation(cancelled.clone());
        let worker_cancelled = cancelled.clone();
        let thread = thread::Builder::new()
            .name("voicetype-google".into())
            .spawn(move || {
                let mut attempt: Option<GoogleAttempt> = None;
                while let Ok(request) = input.recv() {
                    match request {
                        Request::Start(key) => {
                            if attempt.is_some() {
                                continue;
                            }
                            if worker_cancelled.load(Ordering::Acquire) {
                                if boundary.cleanup().is_ok() {
                                    let _ = output.send((key, ProviderEvent::Released));
                                } else {
                                    let _ = output.send((
                                        key,
                                        ProviderEvent::Failed(SessionFailure::ProviderFailed),
                                    ));
                                    attempt = Some(GoogleAttempt { finished: true });
                                }
                                continue;
                            }
                            match GoogleAttempt::start(&mut boundary) {
                                Ok(started) if !worker_cancelled.load(Ordering::Acquire) => {
                                    attempt = Some(started);
                                    let _ = output.send((key, ProviderEvent::Recording));
                                }
                                Ok(mut started) => {
                                    if started.cancel(&mut boundary).is_ok() {
                                        let _ = output.send((key, ProviderEvent::Released));
                                    } else {
                                        attempt = Some(started);
                                    }
                                }
                                Err(_) => {
                                    let _ = output.send((
                                        key,
                                        ProviderEvent::Failed(SessionFailure::ProviderFailed),
                                    ));
                                    if boundary.cleanup().is_ok() {
                                        let _ = output.send((key, ProviderEvent::Released));
                                    } else {
                                        attempt = Some(GoogleAttempt { finished: true });
                                    }
                                }
                            }
                        }
                        Request::Stop(key) => {
                            if let Some(mut started) = attempt.take() {
                                match started
                                    .finish_with_cancel(&mut boundary, &worker_cancelled)
                                    .and_then(&format)
                                {
                                    Ok(text) => {
                                        let _ = output.send((key, ProviderEvent::Final(text)));
                                    }
                                    Err(_) if !worker_cancelled.load(Ordering::Acquire) => {
                                        let _ = output.send((
                                            key,
                                            ProviderEvent::Failed(SessionFailure::ProviderFailed),
                                        ));
                                    }
                                    Err(_) => {}
                                }
                                if boundary.cleanup().is_ok() {
                                    let _ = output.send((key, ProviderEvent::Released));
                                } else {
                                    attempt = Some(started);
                                }
                            }
                        }
                        Request::Cancel(key) => {
                            if let Some(mut started) = attempt.take() {
                                if started.cancel(&mut boundary).is_ok() {
                                    let _ = output.send((key, ProviderEvent::Released));
                                } else {
                                    let _ = output.send((
                                        key,
                                        ProviderEvent::Failed(SessionFailure::ProviderFailed),
                                    ));
                                    attempt = Some(started);
                                }
                            }
                        }
                        Request::Shutdown => {
                            if let Some(mut started) = attempt.take() {
                                let _ = started.cancel(&mut boundary);
                            }
                            // A failed cleanup still owns a possible live recorder.
                            // Keep the boundary in this worker until release is
                            // confirmed, even if the provider has been dropped.
                            while boundary.cleanup().is_err() {
                                thread::sleep(Duration::from_millis(50));
                            }
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            requests,
            events,
            active: None,
            cancelled,
            thread: Some(thread),
        })
    }

    pub fn recv_timeout(&mut self, budget: Duration) -> Option<(SessionKey, ProviderEvent)> {
        let event = self.events.recv_timeout(budget).ok()?;
        if Some(event.0) == self.active && matches!(event.1, ProviderEvent::Released) {
            self.active = None;
        }
        Some(event)
    }

    pub fn try_event(&mut self) -> Option<(SessionKey, ProviderEvent)> {
        let event = self.events.try_recv().ok()?;
        if Some(event.0) == self.active && matches!(event.1, ProviderEvent::Released) {
            self.active = None;
        }
        Some(event)
    }
}

impl ProviderPort for GoogleProvider {
    fn send(&mut self, command: ProviderCommand) -> Result<(), CommandRejected> {
        let is_start = matches!(command, ProviderCommand::Start { .. });
        let request = match command {
            ProviderCommand::Start { key, .. }
                if key.provider() == Provider::Google && self.active.is_none() =>
            {
                // Reset before accepting this Start. A later accepted Cancel
                // must remain visible even if the worker has not dequeued Start.
                self.cancelled.store(false, Ordering::Release);
                self.active = Some(key);
                Request::Start(key)
            }
            ProviderCommand::Stop { key } if self.active == Some(key) => Request::Stop(key),
            ProviderCommand::Cancel { key } if self.active == Some(key) => {
                self.cancelled.store(true, Ordering::Release);
                Request::Cancel(key)
            }
            _ => return Err(CommandRejected),
        };
        if self.requests.try_send(request).is_err() {
            if is_start {
                self.active = None;
            }
            return Err(CommandRejected);
        }
        Ok(())
    }
}

impl Drop for GoogleProvider {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        let _ = self.requests.send(Request::Shutdown);
        if let Some(thread) = self.thread.take() {
            let until = Instant::now() + Duration::from_secs(2);
            while !thread.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
            // Otherwise dropping JoinHandle detaches the still-running worker.
            // It retains the boundary and retries cleanup; it does not emit a
            // false Released event or abandon a live recorder.
        }
    }
}
