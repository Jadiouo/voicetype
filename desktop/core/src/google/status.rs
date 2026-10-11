// Only bounded, volatile status-match state is retained. CLI output and draft
// text never leave the PTY reader as strings or enter a diagnostic/log.
const STATUS_LIMIT: usize = 160;
const CONNECTING: &[u8] = b"Connecting";

pub(super) struct VoiceStatusProbe {
    generation: u64,
    connecting_at: usize,
    connected: bool,
    failed: bool,
    ready: bool,
    bullet_at: usize,
    anchored: bool,
    status: [u8; STATUS_LIMIT],
    status_len: usize,
    ansi: Ansi,
}

impl Default for VoiceStatusProbe {
    fn default() -> Self {
        Self {
            generation: 0,
            connecting_at: 0,
            connected: false,
            failed: false,
            ready: false,
            bullet_at: 0,
            anchored: false,
            status: [0; STATUS_LIMIT],
            status_len: 0,
            ansi: Ansi::Plain,
        }
    }
}

#[derive(Default)]
enum Ansi {
    #[default]
    Plain,
    Escape,
    Csi,
    Osc,
    OscEscape,
}

impl VoiceStatusProbe {
    pub(super) fn arm(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.connecting_at = 0;
        self.connected = false;
        self.failed = false;
        self.ready = false;
        self.bullet_at = 0;
        self.anchored = false;
        self.status.fill(0);
        self.status_len = 0;
        self.ansi = Ansi::Plain;
        self.generation
    }

    pub(super) fn fail(&mut self) {
        self.failed = true;
        self.ready = false;
        self.status.fill(0);
        self.status_len = 0;
    }

    pub(super) fn ready(&self, generation: u64) -> bool {
        generation == self.generation && self.ready && !self.failed
    }

    pub(super) fn feed(&mut self, bytes: &[u8]) {
        if self.generation == 0 || self.failed || self.ready {
            return;
        }
        for &byte in bytes {
            match self.ansi {
                Ansi::Plain if byte == 0x1b => self.ansi = Ansi::Escape,
                Ansi::Plain => self.visible(byte),
                Ansi::Escape if byte == b'[' => self.ansi = Ansi::Csi,
                Ansi::Escape if byte == b']' => self.ansi = Ansi::Osc,
                Ansi::Escape => {
                    self.ansi = Ansi::Plain;
                    self.status_len = 0;
                    self.anchored = false;
                }
                Ansi::Csi if (0x40..=0x7e).contains(&byte) => {
                    self.ansi = Ansi::Plain;
                    // SGR only changes styling. Cursor movement/redraw starts
                    // another screen position, so a status cannot span it.
                    if byte != b'm' {
                        self.status_len = 0;
                        self.bullet_at = 0;
                        self.anchored = false;
                    }
                }
                Ansi::Csi => {}
                Ansi::Osc if byte == 7 => self.ansi = Ansi::Plain,
                Ansi::Osc if byte == 0x1b => self.ansi = Ansi::OscEscape,
                Ansi::Osc => {}
                Ansi::OscEscape if byte == b'\\' => self.ansi = Ansi::Plain,
                Ansi::OscEscape => self.ansi = Ansi::Osc,
            }
        }
    }

    fn visible(&mut self, byte: u8) {
        if byte == b'\r' || byte == b'\n' {
            self.status_len = 0;
            self.bullet_at = 0;
            self.anchored = false;
            return;
        }
        if byte == CONNECTING[self.connecting_at] {
            self.connecting_at += 1;
            if self.connecting_at == CONNECTING.len() {
                self.connected = true;
                self.connecting_at = 0;
                // A Recording render that preceded Connecting is never proof
                // of this session's state transition.
                self.anchored = false;
                self.status_len = 0;
            }
        } else {
            self.connecting_at = usize::from(byte == CONNECTING[0]);
        }

        // The CLI's status bullet is the anchor; arbitrary prompt text that
        // happens to say "Recording" cannot assert microphone readiness.
        self.bullet_at = match (self.bullet_at, byte) {
            (0, 0xe2) => 1,
            (1, 0x97) => 2,
            (2, 0x8f) => {
                self.status_len = 0;
                self.anchored = true;
                0
            }
            (_, 0xe2) => 1,
            _ => 0,
        };
        if !self.anchored || (byte == 0x8f && self.status_len == 0) {
            // Keep only printable ASCII following the status bullet. Other
            // UTF-8 decoration (ellipsis, middle dot) is ignored.
            return;
        }
        if self.status_len == 0 && byte != b' ' && byte != b'R' {
            return;
        }
        if (0x20..=0x7e).contains(&byte) && self.status_len < STATUS_LIMIT {
            self.status[self.status_len] = byte;
            self.status_len += 1;
            self.ready = self.connected && recording_status(&self.status[..self.status_len]);
        }
        if self.status_len == STATUS_LIMIT {
            self.status_len = 0;
        }
    }
}

fn recording_status(bytes: &[u8]) -> bool {
    let Some(mut rest) = bytes.trim_ascii_start().strip_prefix(b"Recording ") else {
        return false;
    };
    let minute_digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
    if !(1..=2).contains(&minute_digits) || rest.get(minute_digits) != Some(&b':') {
        return false;
    }
    rest = &rest[minute_digits + 1..];
    if rest.len() < 2 || !rest[..2].iter().all(u8::is_ascii_digit) {
        return false;
    }
    if (rest[0] - b'0') * 10 + (rest[1] - b'0') >= 60 {
        return false;
    }
    rest = &rest[2..];
    if !rest.first().is_some_and(u8::is_ascii_whitespace) {
        return false;
    }
    rest = rest.trim_ascii_start();
    let Some(after_insert) = rest.strip_prefix(b"enter or f5 to insert") else {
        return false;
    };
    if !after_insert.first().is_some_and(u8::is_ascii_whitespace) {
        return false;
    }
    after_insert
        .trim_ascii_start()
        .starts_with(b"esc to discard")
}

#[cfg(test)]
mod tests {
    use super::VoiceStatusProbe;

    #[test]
    fn connecting_then_full_recording_status_is_ready_across_ansi_utf8_chunks() {
        let mut probe = VoiceStatusProbe::default();
        let generation = probe.arm();
        for chunk in [
            &b"\x1b[2JConnecting\xe2"[..],
            &b"\x80\xa6\r\x1b[1;1H\xe2\x97"[..],
            &b"\x8f \x1b[32mRecording 0:07\x1b[0m  enter or f5 to insert \xc2"[..],
            &b"\xb7 esc to discard"[..],
        ] {
            probe.feed(chunk);
        }
        assert!(probe.ready(generation));
    }

    #[test]
    fn f5_without_recording_or_connecting_then_error_is_not_ready() {
        let mut probe = VoiceStatusProbe::default();
        let generation = probe.arm();
        probe.feed(b"Connecting...\rMicrophone access denied\r");
        assert!(!probe.ready(generation));
        probe.fail();
        probe.feed(b"\xe2\x97\x8f Recording 0:07");
        assert!(!probe.ready(generation));
    }

    #[test]
    fn new_generation_rejects_late_previous_recording_render() {
        let mut probe = VoiceStatusProbe::default();
        let old = probe.arm();
        probe.feed(b"Connecting...\r\xe2\x97\x8f Recording 0:07  enter or f5 to insert \xc2\xb7 esc to discard");
        assert!(probe.ready(old));
        let current = probe.arm();
        assert!(!probe.ready(old));
        probe.feed(b"\r\xe2\x97\x8f Recording 0:08  enter or f5 to insert \xc2\xb7 esc to discard");
        assert!(!probe.ready(current));
    }

    #[test]
    fn recording_render_before_connecting_cannot_satisfy_transition() {
        let mut probe = VoiceStatusProbe::default();
        let generation = probe.arm();
        probe.feed(b"\xe2\x97\x8f Recording 0:07  enter or f5 to insert \xc2\xb7 esc to discard");
        probe.feed(b"Connecting...");
        assert!(!probe.ready(generation));
    }

    #[test]
    fn repaint_discards_partial_status_and_requires_complete_valid_timer() {
        let mut probe = VoiceStatusProbe::default();
        let generation = probe.arm();
        probe.feed(b"Connecting...\r\xe2\x97\x8f Recording 0:");
        probe.feed(
            b"\x1b[1;1H\xe2\x97\x8f Recording 0:99  enter or f5 to insert \xc2\xb7 esc to discard",
        );
        assert!(!probe.ready(generation));
        probe.feed(
            b"\x1b[1;1H\xe2\x97\x8f Recording 0:07  enter or f5 to insert \xc2\xb7 esc to discard",
        );
        assert!(probe.ready(generation));
    }
}
