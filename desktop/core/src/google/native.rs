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
    time::Duration,
};

pub struct GoogleNativeBoundary {
    terminal: GoogleTerminal,
    budget: Duration,
    cancelled: Option<Arc<AtomicBool>>,
    recording: bool,
    closed: bool,
}

impl GoogleNativeBoundary {
    /// The official CLI must be logged in and able to call the external editor
    /// with an empty draft before this boundary is handed to a provider.
    pub fn open(launch: TerminalLaunch, budget: Duration) -> io::Result<Self> {
        let mut terminal = GoogleTerminal::launch(launch)?;
        if !terminal.capture_editor(budget)?.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "nonempty official CLI draft",
            ));
        }
        Ok(Self {
            terminal,
            budget,
            cancelled: None,
            recording: false,
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
        if self.closed || self.recording {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if self
            .cancelled
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
        {
            return Err(io::ErrorKind::Interrupted.into());
        }
        self.terminal.control(TerminalControl::VoiceToggle)?;
        self.recording = true;
        Ok(())
    }

    fn stop_capture_and_drain(&mut self) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    fn stop_official_voice(&mut self) -> io::Result<()> {
        if !self.recording || self.closed {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        self.terminal.control(TerminalControl::VoiceToggle)?;
        self.recording = false;
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
        if self.recording {
            let _ = self.terminal.control(TerminalControl::Discard);
        }
        self.terminal.shutdown()?;
        self.recording = false;
        self.closed = true;
        Ok(())
    }
}
