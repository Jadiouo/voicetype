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
    terminal: Option<GoogleTerminal>,
    budget: Duration,
    cancelled: Option<Arc<AtomicBool>>,
    // F5 has been sent, so cleanup must request Esc even before the CLI
    // confirms Recording. This is not a mic-ready assertion.
    voice_requested: bool,
    closed: bool,
}

fn cleanup_gate(closed: bool, unpublished_cleanup_pending: bool) -> io::Result<bool> {
    if unpublished_cleanup_pending {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    Ok(closed)
}

fn unpublished_cleanup_pending() -> bool {
    #[cfg(test)]
    if TEST_UNPUBLISHED_PENDING.load(std::sync::atomic::Ordering::Acquire) {
        return true;
    }
    #[cfg(windows)]
    {
        super::terminal::unpublished_cleanup_pending()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
static TEST_UNPUBLISHED_PENDING: AtomicBool = AtomicBool::new(false);

impl GoogleNativeBoundary {
    /// The official CLI must be logged in and able to call the external editor
    /// with an empty draft before this boundary is handed to a provider.
    pub fn open(launch: TerminalLaunch, budget: Duration) -> io::Result<Self> {
        #[cfg(windows)]
        if super::terminal::unpublished_cleanup_pending() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let mut terminal = GoogleTerminal::launch(launch.clone())?;
        match terminal.capture_editor(budget) {
            Ok(draft) if draft.is_empty() => {}
            Ok(_) => {
                terminal.retire_unpublished();
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "nonempty official CLI draft",
                ));
            }
            Err(error) => {
                terminal.retire_unpublished();
                return Err(error);
            }
        }
        Ok(Self {
            launch,
            terminal: Some(terminal),
            budget,
            cancelled: None,
            voice_requested: false,
            closed: false,
        })
    }

    fn terminal_mut(&mut self) -> io::Result<&mut GoogleTerminal> {
        self.terminal
            .as_mut()
            .ok_or(io::ErrorKind::BrokenPipe.into())
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
        if self.closed || self.terminal.is_none() || self.voice_requested {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if self
            .cancelled
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
        {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let generation = self.terminal_mut()?.arm_voice_status()?;
        self.terminal_mut()?.control(TerminalControl::VoiceToggle)?;
        self.voice_requested = true;
        let until = Instant::now() + self.budget.min(Duration::from_secs(6));
        loop {
            if self
                .cancelled
                .as_ref()
                .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
            {
                self.terminal_mut()?.fail_voice_status()?;
                return Err(io::ErrorKind::Interrupted.into());
            }
            if self.terminal_mut()?.child_exit_observed()? {
                self.terminal_mut()?.fail_voice_status()?;
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            match reject_known_voice_errors(self.terminal_mut()?.log_path()) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    self.terminal_mut()?.fail_voice_status()?;
                    return Err(error);
                }
            }
            if self.terminal_mut()?.voice_status_ready(generation)? {
                return Ok(());
            }
            if Instant::now() >= until {
                self.terminal_mut()?.fail_voice_status()?;
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
        if !self.voice_requested || self.closed || self.terminal.is_none() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        self.terminal_mut()?.control(TerminalControl::VoiceToggle)?;
        self.voice_requested = false;
        Ok(())
    }

    fn wait_official_recorder(&mut self) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    fn capture_editor(&mut self) -> io::Result<String> {
        let budget = self.budget;
        self.terminal_mut()?.capture_editor(budget)
    }

    fn settle(&mut self, duration: Duration) -> io::Result<()> {
        thread::sleep(duration);
        Ok(())
    }

    fn reject_known_voice_errors(&mut self) -> io::Result<()> {
        reject_known_voice_errors(self.terminal_mut()?.log_path())
    }

    fn cleanup(&mut self) -> io::Result<()> {
        if cleanup_gate(self.closed, unpublished_cleanup_pending())? {
            return Ok(());
        }
        // Esc requests cancellation only for an unfinished recording; a
        // successfully stopped draft has already been captured twice.
        // Terminal shutdown reaps the Job even if this control fails.
        if self.voice_requested {
            let _ = self.terminal_mut()?.control(TerminalControl::Discard);
        }
        self.terminal_mut()?.shutdown()?;
        cleanup_gate(false, unpublished_cleanup_pending())?;
        self.voice_requested = false;
        // `GoogleAttempt::finish` checks the official error log after cleanup.
        // Retain the closed terminal's session directory until that check.
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

#[cfg(test)]
mod cleanup_tests {
    use super::*;

    #[test]
    fn closed_old_cli_cannot_release_owner_while_failed_prewarm_cleanup_is_pending() {
        let mut boundary = GoogleNativeBoundary {
            launch: TerminalLaunch {
                executable: Default::default(),
                sha256: String::new(),
                workspace: Default::default(),
                profile: Default::default(),
                editor: Default::default(),
            },
            terminal: None,
            budget: Duration::ZERO,
            cancelled: None,
            voice_requested: false,
            closed: true,
        };
        TEST_UNPUBLISHED_PENDING.store(true, std::sync::atomic::Ordering::Release);
        assert_eq!(
            boundary.cleanup().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        TEST_UNPUBLISHED_PENDING.store(false, std::sync::atomic::Ordering::Release);
        boundary.cleanup().unwrap();
    }
}
