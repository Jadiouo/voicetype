//! Concrete official TUI boundary. Audio implementations must prove that all
//! PCM was consumed before this module sends its one stopping F5.
use super::{
    terminal::{GoogleTerminal, TerminalControl, TerminalLaunch},
    GoogleBoundary,
};
use std::{
    ffi::OsString,
    fs, io,
    sync::{atomic::AtomicBool, Arc},
    thread,
    time::Duration,
};

/// Owns one native capture and its authenticated PCM relay. Implementations
/// must not return `stop_and_drain` until recorder stdout reaches EOF, its
/// bounded FIFO is empty, and the official recorder has acknowledged all PCM.
pub trait GoogleAudio {
    fn terminal_environment(&self) -> Vec<(OsString, OsString)> {
        Vec::new()
    }
    fn set_cancellation(&mut self, _flag: Arc<AtomicBool>) {}
    fn start(&mut self) -> io::Result<()>;
    fn wait_connected(&mut self) -> io::Result<()>;
    fn stop_and_drain(&mut self) -> io::Result<()>;
    fn wait_recorder_exit(&mut self) -> io::Result<()>;
    fn cleanup(&mut self) -> io::Result<()>;
}

pub struct GoogleCliBoundary<A: GoogleAudio> {
    terminal: GoogleTerminal,
    audio: A,
    budget: Duration,
    cleaned: bool,
}

impl<A: GoogleAudio> GoogleCliBoundary<A> {
    /// An empty external-editor capture is the only accepted readiness probe.
    /// A reviewed binary on disk alone is never treated as a login or mic-ready
    /// signal. This opens no microphone and sends no prompt.
    pub fn open(launch: TerminalLaunch, audio: A, budget: Duration) -> io::Result<Self> {
        let mut terminal = GoogleTerminal::launch_with_env(launch, &audio.terminal_environment())?;
        if !terminal.capture_editor(budget)?.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "nonempty official CLI draft",
            ));
        }
        Ok(Self {
            terminal,
            audio,
            budget,
            cleaned: false,
        })
    }
}

impl<A: GoogleAudio> GoogleBoundary for GoogleCliBoundary<A> {
    fn set_cancellation(&mut self, flag: Arc<AtomicBool>) {
        self.audio.set_cancellation(flag);
    }

    fn start_capture(&mut self) -> io::Result<()> {
        if self.cleaned {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        self.audio.start()?;
        self.terminal.control(TerminalControl::VoiceToggle)?;
        self.audio.wait_connected()
    }

    fn stop_capture_and_drain(&mut self) -> io::Result<()> {
        self.audio.stop_and_drain()
    }
    fn stop_official_voice(&mut self) -> io::Result<()> {
        self.terminal.control(TerminalControl::VoiceToggle)
    }
    fn wait_official_recorder(&mut self) -> io::Result<()> {
        self.audio.wait_recorder_exit()
    }
    fn capture_editor(&mut self) -> io::Result<String> {
        self.terminal.capture_editor(self.budget)
    }
    fn settle(&mut self, duration: Duration) -> io::Result<()> {
        thread::sleep(duration);
        Ok(())
    }

    fn reject_known_voice_errors(&mut self) -> io::Result<()> {
        let path = self.terminal.log_path();
        let meta = fs::symlink_metadata(&path)?;
        if !meta.file_type().is_file() || meta.len() > 4 * 1024 * 1024 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let bytes = fs::read(path)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        for line in bytes.split(|byte| *byte == b'\n') {
            let line = line.iter().map(u8::to_ascii_lowercase).collect::<Vec<_>>();
            let source = [b"audio:".as_slice(), b"voice:", b"mic:"]
                .iter()
                .any(|prefix| line.windows(prefix.len()).any(|w| w == *prefix));
            let failure = [
                b"failed".as_slice(),
                b"error",
                b"cancelled",
                b"canceled",
                b"timeout",
                b"timed out",
            ]
            .iter()
            .any(|word| line.windows(word.len()).any(|w| w == *word));
            if source && failure {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "official voice failure",
                ));
            }
        }
        Ok(())
    }

    fn cleanup(&mut self) -> io::Result<()> {
        if self.cleaned {
            return Ok(());
        }
        let audio = self.audio.cleanup();
        let terminal = self.terminal.shutdown();
        audio?;
        terminal?;
        self.cleaned = true;
        Ok(())
    }
}
