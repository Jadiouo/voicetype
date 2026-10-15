//! Windows v1 automatic delivery: `SendInput` with `KEYEVENTF_UNICODE`.
//!
//! This is best-effort, not atomic: the target is re-verified immediately before
//! one single injection, but a focus change inside that millisecond window can
//! still misdirect text. Every doubtful path keeps the full text for manual copy
//! (`NotSent` / `FocusChanged` / `Unconfirmed` / `Partial`) and nothing is ever
//! retried. Transcript text is never logged.
use crate::{DeliveryOutcome, DeliveryPort, TargetLease};

/// Upper bound on UTF-16 code units per delivery; longer text is kept, not split.
pub const MAX_UNITS: usize = 4000;
/// How long to wait for the user to release Ctrl/Shift/Alt/Win after Stop.
pub const MODIFIER_WAIT_MS: u64 = 300;
const MODIFIER_POLL_MS: u64 = 10;
/// `dwExtraInfo` tag for events injected by VoiceType.
pub const EXTRA_INFO: usize = 0x5654_5950;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    Empty,
    Control,
    TooLong,
}

/// Convert text to the UTF-16 code units to type. Line breaks and tabs become one
/// space (a bare Enter could submit a chat box or run a terminal command); other
/// control characters reject the whole text. Surrogate pairs stay as two units.
pub fn utf16_units(text: &str) -> Result<Vec<u16>, Reject> {
    let mut units = Vec::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                units.push(b' ' as u16);
            }
            '\n' | '\t' => units.push(b' ' as u16),
            c if c.is_control() => return Err(Reject::Control),
            c => {
                let mut buf = [0u16; 2];
                units.extend_from_slice(c.encode_utf16(&mut buf));
            }
        }
        if units.len() > MAX_UNITS {
            return Err(Reject::TooLong);
        }
    }
    if units.iter().all(|u| *u == b' ' as u16) {
        return Err(Reject::Empty);
    }
    Ok(units)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetState {
    /// Same focus as the lease and allowed (not elevated, not UWP, not x86).
    Ready,
    /// Focus differs from the lease or cannot be determined.
    Changed,
    /// Same focus but unsupported (elevated / AppContainer / WOW64 / unknown).
    Denied,
}

pub trait Env {
    fn target_state(&mut self, target: &TargetLease) -> TargetState;
    fn modifiers_down(&mut self) -> bool;
    fn sleep_ms(&mut self, ms: u64);
}

pub trait Injector {
    /// Insert key-down/key-up pairs for each unit in a single call. Returns the
    /// number of input events the OS accepted (expected: `2 * units.len()`).
    fn send(&mut self, units: &[u16]) -> usize;
}

/// Order: format -> modifier release -> target re-check -> one injection.
pub fn deliver(
    env: &mut dyn Env,
    injector: &mut dyn Injector,
    target: &TargetLease,
    text: &str,
) -> DeliveryOutcome {
    let Ok(units) = utf16_units(text) else {
        return DeliveryOutcome::NotSent;
    };
    let mut waited = 0;
    while env.modifiers_down() {
        if waited >= MODIFIER_WAIT_MS {
            return DeliveryOutcome::NotSent;
        }
        env.sleep_ms(MODIFIER_POLL_MS);
        waited += MODIFIER_POLL_MS;
    }
    match env.target_state(target) {
        TargetState::Ready => {}
        TargetState::Changed => return DeliveryOutcome::FocusChanged,
        TargetState::Denied => return DeliveryOutcome::NotSent,
    }
    let expected = units.len() * 2;
    match injector.send(&units) {
        n if n == expected => DeliveryOutcome::Delivered,
        // UIPI blocking reports neither an error nor a distinct count, so zero
        // cannot be treated as "nothing happened".
        0 => DeliveryOutcome::Unconfirmed,
        _ => DeliveryOutcome::Partial,
    }
}

/// `DeliveryPort` over any `Env` + `Injector` (fakes in tests, Win32 in the app).
pub struct Delivery<E, I> {
    pub env: E,
    pub injector: I,
}

impl<E: Env, I: Injector> DeliveryPort for Delivery<E, I> {
    fn commit_if_focused(&mut self, target: &TargetLease, text: &str) -> DeliveryOutcome {
        deliver(&mut self.env, &mut self.injector, target, text)
    }
}

#[cfg(windows)]
pub use win32::{capture_target, target_state, SendInputInjector, Win32Delivery, Win32Env};

#[cfg(windows)]
mod win32 {
    use super::*;
    use std::{mem::size_of, thread, time::Duration};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        Security::{
            GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel,
            TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
        },
        System::Threading::{
            GetCurrentProcess, IsWow64Process, OpenProcess, OpenProcessToken,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
                KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN,
                VK_SHIFT,
            },
            WindowsAndMessaging::{
                GetClassNameW, GetForegroundWindow, GetGUIThreadInfo, GetWindowLongW,
                GetWindowThreadProcessId, GUITHREADINFO, GWL_STYLE,
            },
        },
    };

    const ES_PASSWORD: i32 = 0x20;
    /// TOKEN_INFORMATION_CLASS value for TokenIsAppContainer.
    const TOKEN_APPCONTAINER: i32 = 29; // TokenIsAppContainer

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    fn token_u32(token: HANDLE, class: i32) -> Option<u32> {
        let mut value = 0u32;
        let mut len = 0u32;
        let ok = unsafe {
            GetTokenInformation(token, class, &mut value as *mut _ as *mut _, 4, &mut len)
        };
        (ok != 0).then_some(value)
    }

    fn integrity_rid(token: HANDLE) -> Option<u32> {
        let mut len = 0u32;
        unsafe {
            GetTokenInformation(
                token,
                TokenIntegrityLevel,
                std::ptr::null_mut(),
                0,
                &mut len,
            )
        };
        if len == 0 || len > 1024 {
            return None;
        }
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenIntegrityLevel,
                buf.as_mut_ptr() as *mut _,
                len,
                &mut len,
            )
        };
        if ok == 0 {
            return None;
        }
        let label = unsafe { &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL) };
        let sid = label.Label.Sid;
        if sid.is_null() {
            return None;
        }
        let count = unsafe { *GetSidSubAuthorityCount(sid) };
        if count == 0 {
            return None;
        }
        Some(unsafe { *GetSidSubAuthority(sid, count as u32 - 1) })
    }

    /// Supported = integrity not above ours (so an elevated target is refused from a
    /// normal App, which is what UIPI would block silently), not AppContainer, not
    /// 32-bit. Any query failure (including access denied) means unsupported.
    fn process_supported(pid: u32) -> bool {
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return false;
        }
        let process = Handle(process);
        let mut wow64 = 0;
        if unsafe { IsWow64Process(process.0, &mut wow64) } == 0 || wow64 != 0 {
            return false;
        }
        let mut token: HANDLE = std::ptr::null_mut();
        if unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) } == 0 {
            return false;
        }
        let token = Handle(token);
        let mut own: HANDLE = std::ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut own) } == 0 {
            return false;
        }
        let own = Handle(own);
        if token_u32(token.0, TOKEN_APPCONTAINER) != Some(0) {
            return false;
        }
        match (integrity_rid(token.0), integrity_rid(own.0)) {
            (Some(target), Some(ours)) => target <= ours,
            _ => false,
        }
    }

    struct Focus {
        window: usize,
        focus: usize,
        thread: u32,
        pids: [u32; 2],
        class: String,
        style: i32,
    }

    fn focus_now() -> Option<Focus> {
        let window = unsafe { GetForegroundWindow() };
        if window.is_null() {
            return None;
        }
        let mut gui = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetGUIThreadInfo(0, &mut gui) } == 0 || gui.hwndFocus.is_null() {
            return None;
        }
        let mut front_pid = 0u32;
        unsafe { GetWindowThreadProcessId(window, &mut front_pid) };
        let mut focus_pid = 0u32;
        let thread = unsafe { GetWindowThreadProcessId(gui.hwndFocus, &mut focus_pid) };
        if front_pid == 0 || focus_pid == 0 {
            return None;
        }
        let mut class = [0u16; 64];
        let n = unsafe { GetClassNameW(gui.hwndFocus, class.as_mut_ptr(), class.len() as i32) };
        let class = if n > 0 {
            String::from_utf16_lossy(&class[..n as usize])
        } else {
            String::new()
        };
        Some(Focus {
            window: window as usize,
            focus: gui.hwndFocus as usize,
            thread,
            pids: [front_pid, focus_pid],
            class,
            style: unsafe { GetWindowLongW(gui.hwndFocus, GWL_STYLE) },
        })
    }

    fn lease_id(f: &Focus) -> String {
        format!("win32:{:x}:{:x}:{:x}", f.window, f.focus, f.thread)
    }

    fn supported(f: &Focus) -> bool {
        // Classic Edit/RichEdit password boxes are refused. Browser password
        // fields cannot be told apart without UI Automation (not in v1).
        let classic = f.class == "Edit" || f.class.starts_with("RichEdit");
        if classic && f.style & ES_PASSWORD != 0 {
            return false;
        }
        f.pids.iter().all(|pid| process_supported(*pid))
    }

    /// Lock the foreground window + focused control, or `None` when nothing is
    /// focused or the target is unsupported (so recording never starts).
    pub fn capture_target() -> Option<TargetLease> {
        let f = focus_now()?;
        supported(&f).then(|| TargetLease::new(lease_id(&f), 0))
    }

    pub fn target_state(target: &TargetLease) -> TargetState {
        match focus_now() {
            Some(f) if lease_id(&f) == target.context_id => {
                if supported(&f) {
                    TargetState::Ready
                } else {
                    TargetState::Denied
                }
            }
            _ => TargetState::Changed,
        }
    }

    pub struct Win32Env;
    impl Env for Win32Env {
        fn target_state(&mut self, target: &TargetLease) -> TargetState {
            target_state(target)
        }
        fn modifiers_down(&mut self) -> bool {
            [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
                .iter()
                .any(|vk| unsafe { GetAsyncKeyState(*vk as i32) } < 0)
        }
        fn sleep_ms(&mut self, ms: u64) {
            thread::sleep(Duration::from_millis(ms));
        }
    }

    pub struct SendInputInjector;
    impl Injector for SendInputInjector {
        fn send(&mut self, units: &[u16]) -> usize {
            let mut inputs = Vec::with_capacity(units.len() * 2);
            for unit in units {
                for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
                    inputs.push(INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 {
                            ki: KEYBDINPUT {
                                wVk: 0,
                                wScan: *unit,
                                dwFlags: flags,
                                time: 0,
                                dwExtraInfo: EXTRA_INFO,
                            },
                        },
                    });
                }
            }
            let sent = unsafe {
                SendInput(
                    inputs.len() as u32,
                    inputs.as_ptr(),
                    size_of::<INPUT>() as i32,
                )
            };
            sent as usize
        }
    }

    pub type Win32Delivery = Delivery<Win32Env, SendInputInjector>;
    impl Win32Delivery {
        pub fn new() -> Self {
            Delivery {
                env: Win32Env,
                injector: SendInputInjector,
            }
        }
    }
    impl Default for Win32Delivery {
        fn default() -> Self {
            Self::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeEnv {
        state: TargetState,
        held_polls: u32,
        sleeps: u64,
        checks: u32,
    }
    impl FakeEnv {
        fn ready() -> Self {
            Self {
                state: TargetState::Ready,
                held_polls: 0,
                sleeps: 0,
                checks: 0,
            }
        }
    }
    impl Env for FakeEnv {
        fn target_state(&mut self, _: &TargetLease) -> TargetState {
            self.checks += 1;
            self.state
        }
        fn modifiers_down(&mut self) -> bool {
            if self.held_polls > 0 {
                self.held_polls -= 1;
                true
            } else {
                false
            }
        }
        fn sleep_ms(&mut self, ms: u64) {
            self.sleeps += ms;
        }
    }
    struct FakeInjector {
        calls: u32,
        reply: Option<usize>,
        last: Vec<u16>,
    }
    impl FakeInjector {
        fn ok() -> Self {
            Self {
                calls: 0,
                reply: None,
                last: vec![],
            }
        }
    }
    impl Injector for FakeInjector {
        fn send(&mut self, units: &[u16]) -> usize {
            self.calls += 1;
            self.last = units.to_vec();
            self.reply.unwrap_or(units.len() * 2)
        }
    }
    fn lease() -> TargetLease {
        TargetLease::new("win32:1:2:3", 1)
    }

    #[test]
    fn mixed_text_and_surrogate_pairs() {
        let u = utf16_units("嗨 hi 😀").unwrap();
        assert_eq!(u, "嗨 hi 😀".encode_utf16().collect::<Vec<_>>());
        assert_eq!(u.len(), 7); // emoji is two units
    }

    #[test]
    fn line_breaks_become_single_space() {
        let u = utf16_units("a\r\nb\nc\rd\te").unwrap();
        assert_eq!(String::from_utf16(&u).unwrap(), "a b c d e");
    }

    #[test]
    fn rejects_controls_empty_and_long() {
        assert_eq!(utf16_units("a\0b"), Err(Reject::Control));
        assert_eq!(utf16_units("a\u{1b}b"), Err(Reject::Control));
        assert_eq!(utf16_units(""), Err(Reject::Empty));
        assert_eq!(utf16_units(" \n\t"), Err(Reject::Empty));
        assert_eq!(
            utf16_units(&"a".repeat(MAX_UNITS)).unwrap().len(),
            MAX_UNITS
        );
        assert_eq!(
            utf16_units(&"a".repeat(MAX_UNITS + 1)),
            Err(Reject::TooLong)
        );
    }

    #[test]
    fn delivers_once() {
        let (mut e, mut i) = (FakeEnv::ready(), FakeInjector::ok());
        assert_eq!(
            deliver(&mut e, &mut i, &lease(), "你好"),
            DeliveryOutcome::Delivered
        );
        assert_eq!(i.calls, 1);
        assert_eq!(i.last.len(), 2);
    }

    #[test]
    fn waits_for_modifiers_then_sends() {
        let (mut e, mut i) = (FakeEnv::ready(), FakeInjector::ok());
        e.held_polls = 5;
        assert_eq!(
            deliver(&mut e, &mut i, &lease(), "x"),
            DeliveryOutcome::Delivered
        );
        assert_eq!(e.sleeps, 50);
    }

    #[test]
    fn modifier_timeout_keeps_text_without_injecting() {
        let (mut e, mut i) = (FakeEnv::ready(), FakeInjector::ok());
        e.held_polls = u32::MAX;
        assert_eq!(
            deliver(&mut e, &mut i, &lease(), "x"),
            DeliveryOutcome::NotSent
        );
        assert_eq!(i.calls, 0);
        assert_eq!(e.checks, 0);
    }

    #[test]
    fn target_changes_and_denial_never_inject() {
        for (state, want) in [
            (TargetState::Changed, DeliveryOutcome::FocusChanged),
            (TargetState::Denied, DeliveryOutcome::NotSent),
        ] {
            let (mut e, mut i) = (FakeEnv::ready(), FakeInjector::ok());
            e.state = state;
            assert_eq!(deliver(&mut e, &mut i, &lease(), "x"), want);
            assert_eq!(i.calls, 0);
        }
    }

    #[test]
    fn bad_text_never_injects() {
        let (mut e, mut i) = (FakeEnv::ready(), FakeInjector::ok());
        assert_eq!(
            deliver(&mut e, &mut i, &lease(), "a\0"),
            DeliveryOutcome::NotSent
        );
        assert_eq!(i.calls, 0);
    }

    #[test]
    fn short_or_zero_injection_is_never_retried() {
        for (reply, want) in [
            (0, DeliveryOutcome::Unconfirmed),
            (3, DeliveryOutcome::Partial),
        ] {
            let (mut e, mut i) = (FakeEnv::ready(), FakeInjector::ok());
            i.reply = Some(reply);
            assert_eq!(deliver(&mut e, &mut i, &lease(), "abc"), want);
            assert_eq!(i.calls, 1);
        }
    }
}
