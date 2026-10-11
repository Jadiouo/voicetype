//! Official CLI-owned microphone path for Windows. The App never sees PCM or
//! a recorder drain acknowledgement; successful editor snapshots are not a
//! claim about byte-perfect capture or sampled-audio review.
use super::{
    adapter::reject_known_voice_errors,
    terminal::{GoogleTerminal, TerminalControl, TerminalLaunch},
    GoogleBoundary, GoogleCaptureOwnership,
};
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
    thread,
    time::{Duration, Instant},
};

pub struct GoogleNativeBoundary {
    launch: TerminalLaunch,
    terminal: GoogleTerminal,
    budget: Duration,
    cancelled: Option<Arc<AtomicBool>>,
    // F5 has been sent, so cleanup must request Esc even before the CLI
    // confirms Recording. This is not a mic-ready assertion.
    voice_requested: bool,
    closed: bool,
}

impl GoogleNativeBoundary {
    /// The official CLI must be logged in and able to call the external editor
    /// with an empty draft before this boundary is handed to a provider.
    pub fn open(launch: TerminalLaunch, budget: Duration) -> io::Result<Self> {
        let mut terminal = GoogleTerminal::launch(launch.clone())?;
        if !terminal.capture_editor(budget)?.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "nonempty official CLI draft",
            ));
        }
        Ok(Self {
            launch,
            terminal,
            budget,
            cancelled: None,
            voice_requested: false,
            closed: false,
        })
    }
}

impl GoogleBoundary for GoogleNativeBoundary {
    fn capture_ownership(&self) -> GoogleCaptureOwnership {
        GoogleCaptureOwnership::OfficialCliNative
    }

    fn set_cancellation(&mut self, flag: Arc<AtomicBool>) {
        self.cancelled = Some(flag);
    }

    fn start_capture(&mut self) -> io::Result<()> {
        if self.closed || self.voice_requested {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if self
            .cancelled
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
        {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let generation = self.terminal.arm_voice_status()?;
        self.terminal.control(TerminalControl::VoiceToggle)?;
        self.voice_requested = true;
        let until = Instant::now() + self.budget.min(Duration::from_secs(6));
        loop {
            if self
                .cancelled
                .as_ref()
                .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
            {
                self.terminal.fail_voice_status()?;
                return Err(io::ErrorKind::Interrupted.into());
            }
            if self.terminal.child_exit_observed()? {
                self.terminal.fail_voice_status()?;
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            match reject_known_voice_errors(self.terminal.log_path()) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    self.terminal.fail_voice_status()?;
                    return Err(error);
                }
            }
            if self.terminal.voice_status_ready(generation)? {
                return Ok(());
            }
            if Instant::now() >= until {
                self.terminal.fail_voice_status()?;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "official CLI recording status not observed",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn stop_capture_and_drain(&mut self) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    fn stop_official_voice(&mut self) -> io::Result<()> {
        if !self.voice_requested || self.closed {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        self.terminal.control(TerminalControl::VoiceToggle)?;
        self.voice_requested = false;
        Ok(())
    }

    fn wait_official_recorder(&mut self) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    fn capture_editor(&mut self) -> io::Result<String> {
        self.terminal.capture_editor(self.budget)
    }

    fn settle(&mut self, duration: Duration) -> io::Result<()> {
        thread::sleep(duration);
        Ok(())
    }

    fn reject_known_voice_errors(&mut self) -> io::Result<()> {
        reject_known_voice_errors(self.terminal.log_path())
    }

    fn cleanup(&mut self) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        // Esc requests cancellation only for an unfinished recording; a
        // successfully stopped draft has already been captured twice.
        // Terminal shutdown reaps the Job even if this control fails.
        if self.voice_requested {
            let _ = self.terminal.control(TerminalControl::Discard);
        }
        self.terminal.shutdown()?;
        self.voice_requested = false;
        self.closed = true;
        Ok(())
    }

    fn prewarm_next(&mut self) -> io::Result<()> {
        if !self.closed {
            return Ok(());
        }
        // The previous terminal's shutdown has observed child exit and ConPTY
        // close. A fresh process/reader makes late old status bytes incapable
        // of satisfying this sentence's readiness generation.
        let mut replacement = Self::open(self.launch.clone(), self.budget)?;
        if let Some(flag) = &self.cancelled {
            replacement.set_cancellation(flag.clone());
        }
        *self = replacement;
        Ok(())
    }
}
