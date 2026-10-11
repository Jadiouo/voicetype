//! Official interactive CLI voice attempt. OS process, recorder and editor
//! operations enter only through `GoogleBoundary`; no command accepts a prompt
//! or an Enter key. This module does not infer a transcript from terminal bytes.
pub mod adapter;
#[cfg(target_os = "linux")]
pub mod legacy_config;
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
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc, Arc, Mutex,
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
        let Ok(digest) = Self::image_digest(&self.executable) else {
            return if fs::symlink_metadata(&self.executable).is_err() {
                GoogleInstallState::Missing
            } else {
                GoogleInstallState::Incompatible
            };
        };
        if self.reviewed_sha256.len() != 64 || digest != self.reviewed_sha256.to_ascii_lowercase() {
            return GoogleInstallState::Incompatible;
        }
        GoogleInstallState::OfficialCheckRequired
    }

    pub(crate) fn image_digest(executable: &Path) -> io::Result<String> {
        let meta = fs::symlink_metadata(executable)?;
        if !executable.is_absolute() || !meta.file_type().is_file() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let bytes = fs::read(executable)?;
        Ok(format!("{:x}", Sha256::digest(&bytes)))
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
/// App integration transfers its `InputLease` into this resident owner before
/// capture. Dropping `GoogleProvider` can detach a cleanup worker; the caller
/// cannot release the microphone lease merely because Drop returns. Keep the
/// lease with the boundary until confirmed cleanup and child reap.
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
    /// Optional complete 16 kHz s16 mono capture for post-delivery review.
    /// The owner may return it only after the recorder reader has reached EOF.
    fn take_review_pcm(&mut self) -> Option<Vec<u8>> {
        None
    }
    /// Prepare a fresh empty CLI after verified cleanup, while idle.
    fn prewarm_next(&mut self) -> io::Result<()> {
        Ok(())
    }
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
}

fn emit_event(
    output: &mpsc::SyncSender<(SessionKey, ProviderEvent)>,
    shutdown: &AtomicBool,
    mut event: (SessionKey, ProviderEvent),
) -> bool {
    loop {
        if shutdown.load(Ordering::Acquire) {
            // The provider/receiver is going away. Do not invent Released or
            // hold up cleanup on a result nobody can accept anymore.
            return false;
        }
        match output.try_send(event) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Full(pending)) => {
                event = pending;
                thread::sleep(Duration::from_millis(5));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
        }
    }
}

fn release_and_prewarm(
    boundary: &mut impl GoogleBoundary,
    output: &mpsc::SyncSender<(SessionKey, ProviderEvent)>,
    shutdown: &AtomicBool,
    warm_state: &AtomicU8,
    key: SessionKey,
) {
    // The old slot is gone now. Expose that before the asynchronous Released
    // event can make the Coordinator idle and accept another Start.
    warm_state.store(1, Ordering::Release);
    if !emit_event(output, shutdown, (key, ProviderEvent::Released))
        || shutdown.load(Ordering::Acquire)
    {
        return;
    }
    warm_state.store(
        if boundary.prewarm_next().is_ok() {
            0
        } else {
            2
        },
        Ordering::Release,
    );
}

/// Resident owner for one official CLI and microphone. It maps the bounded
/// external operation to the shared coordinator events. The OS boundary owns
/// actual recorder/PTY process handles; `Released` follows verified cleanup.
/// Drop is bounded, but a failed cleanup can outlive it on the worker thread;
/// the App input lease is therefore owned by that worker/boundary.
pub struct GoogleProvider {
    requests: mpsc::SyncSender<Request>,
    events: mpsc::Receiver<(SessionKey, ProviderEvent)>,
    active: Option<SessionKey>,
    cancelled: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    review: Arc<Mutex<Option<(SessionKey, String, Vec<u8>)>>>,
    warm_state: Arc<AtomicU8>, // 0 ready, 1 prewarming, 2 failed
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
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = shutdown.clone();
        let review = Arc::new(Mutex::new(None));
        let worker_review = review.clone();
        let warm_state = Arc::new(AtomicU8::new(0));
        let worker_warm_state = warm_state.clone();
        let thread = thread::Builder::new()
            .name("voicetype-google".into())
            .spawn(move || {
                let mut attempt: Option<GoogleAttempt> = None;
                loop {
                    if worker_shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    let request = match input.recv_timeout(Duration::from_millis(25)) {
                        Ok(request) => request,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    match request {
                        Request::Start(key) => {
                            if attempt.is_some() {
                                continue;
                            }
                            if worker_cancelled.load(Ordering::Acquire) {
                                if boundary.cleanup().is_ok() {
                                    release_and_prewarm(
                                        &mut boundary,
                                        &output,
                                        &worker_shutdown,
                                        &worker_warm_state,
                                        key,
                                    );
                                } else {
                                    let _ = emit_event(
                                        &output,
                                        &worker_shutdown,
                                        (
                                            key,
                                            ProviderEvent::Failed(SessionFailure::ProviderFailed),
                                        ),
                                    );
                                    attempt = Some(GoogleAttempt { finished: true });
                                }
                                continue;
                            }
                            match GoogleAttempt::start(&mut boundary) {
                                Ok(started) if !worker_cancelled.load(Ordering::Acquire) => {
                                    attempt = Some(started);
                                    let _ = emit_event(
                                        &output,
                                        &worker_shutdown,
                                        (key, ProviderEvent::Recording),
                                    );
                                }
                                Ok(mut started) => {
                                    if started.cancel(&mut boundary).is_ok() {
                                        release_and_prewarm(
                                            &mut boundary,
                                            &output,
                                            &worker_shutdown,
                                            &worker_warm_state,
                                            key,
                                        );
                                    } else {
                                        attempt = Some(started);
                                    }
                                }
                                Err(_) => {
                                    let _ = emit_event(
                                        &output,
                                        &worker_shutdown,
                                        (
                                            key,
                                            ProviderEvent::Failed(SessionFailure::ProviderFailed),
                                        ),
                                    );
                                    if boundary.cleanup().is_ok() {
                                        release_and_prewarm(
                                            &mut boundary,
                                            &output,
                                            &worker_shutdown,
                                            &worker_warm_state,
                                            key,
                                        );
                                    } else {
                                        attempt = Some(GoogleAttempt { finished: true });
                                    }
                                }
                            }
                        }
                        Request::Stop(key) => {
                            if let Some(mut started) = attempt.take() {
                                let prepared = started
                                    .finish_with_cancel(&mut boundary, &worker_cancelled)
                                    .and_then(|raw| {
                                        let pcm = boundary.take_review_pcm();
                                        let text = format(raw.clone())?;
                                        Ok((text, raw, pcm))
                                    });
                                match prepared {
                                    Ok((text, raw, pcm)) => {
                                        if let Some(pcm) = pcm {
                                            if let Ok(mut pending) = worker_review.lock() {
                                                *pending = Some((key, raw, pcm));
                                            }
                                        }
                                        if !emit_event(
                                            &output,
                                            &worker_shutdown,
                                            (key, ProviderEvent::Final(text)),
                                        ) {
                                            if let Ok(mut pending) = worker_review.lock() {
                                                *pending = None;
                                            }
                                        }
                                    }
                                    Err(_) if !worker_cancelled.load(Ordering::Acquire) => {
                                        let _ = emit_event(
                                            &output,
                                            &worker_shutdown,
                                            (
                                                key,
                                                ProviderEvent::Failed(
                                                    SessionFailure::ProviderFailed,
                                                ),
                                            ),
                                        );
                                    }
                                    Err(_) => {}
                                }
                                if boundary.cleanup().is_ok() {
                                    release_and_prewarm(
                                        &mut boundary,
                                        &output,
                                        &worker_shutdown,
                                        &worker_warm_state,
                                        key,
                                    );
                                } else {
                                    attempt = Some(started);
                                }
                            }
                        }
                        Request::Cancel(key) => {
                            if let Some(mut started) = attempt.take() {
                                if started.cancel(&mut boundary).is_ok() {
                                    release_and_prewarm(
                                        &mut boundary,
                                        &output,
                                        &worker_shutdown,
                                        &worker_warm_state,
                                        key,
                                    );
                                } else {
                                    let _ = emit_event(
                                        &output,
                                        &worker_shutdown,
                                        (
                                            key,
                                            ProviderEvent::Failed(SessionFailure::ProviderFailed),
                                        ),
                                    );
                                    attempt = Some(started);
                                }
                            }
                        }
                    }
                }
                // Shutdown and sender disconnection share the same verified
                // cleanup path. A disconnect can arrive after the final flag
                // check if Drop's bounded wait expires before recv returns.
                if let Some(mut started) = attempt.take() {
                    let _ = started.cancel(&mut boundary);
                }
                while boundary.cleanup().is_err() {
                    thread::sleep(Duration::from_millis(50));
                }
            })?;
        Ok(Self {
            requests,
            events,
            active: None,
            cancelled,
            shutdown,
            thread: Some(thread),
            review,
            warm_state,
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

    /// One complete capture, exposed only to the post-delivery review gate.
    pub fn take_review_sample(&mut self, key: SessionKey) -> Option<(String, Vec<u8>)> {
        let mut pending = self.review.lock().ok()?;
        match pending.take() {
            Some((sample_key, raw, pcm)) if sample_key == key => Some((raw, pcm)),
            _ => None,
        }
    }

    pub fn warm_state(&self) -> u8 {
        self.warm_state.load(Ordering::Acquire)
    }
}

impl ProviderPort for GoogleProvider {
    fn send(&mut self, command: ProviderCommand) -> Result<(), CommandRejected> {
        let is_start = matches!(command, ProviderCommand::Start { .. });
        if is_start && self.warm_state() == 2 {
            return Err(CommandRejected);
        }
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
        // Signal independently of the bounded request queue: a stalled CLI can
        // leave every request slot occupied while its owner is being dropped.
        self.shutdown.store(true, Ordering::Release);
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
