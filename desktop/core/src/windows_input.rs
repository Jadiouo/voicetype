//! Temporary recovery-only finite shortcut toggle. This hook consumes the chord;
//! it must be replaced by TSF's preserved-key route before automatic delivery.
use crate::windows_gesture::{shortcut_matches, GestureAction, WindowsGesture};
use crate::{ShortcutChoice, TargetLease};
use std::{
    io,
    mem::size_of,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{LPARAM, LRESULT, WPARAM},
    System::Threading::GetCurrentThreadId,
    UI::{
        Input::KeyboardAndMouse::{
            VK_CAPITAL, VK_CONTROL, VK_ESCAPE, VK_F9, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN,
            VK_MENU, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
        },
        WindowsAndMessaging::{
            CallNextHookEx, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo, GetMessageW,
            GetWindowLongW, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
            UnhookWindowsHookEx, GUITHREADINFO, GWL_STYLE, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG,
            PM_NOREMOVE, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
        },
    },
};

#[derive(Clone, Debug)]
pub enum InputEvent {
    Start(TargetLease),
    Stop,
    Cancel,
}

#[derive(Default)]
struct HookState {
    tx: Option<Sender<InputEvent>>,
    choice: ShortcutChoice,
    enabled: bool,
    ctrl_left: bool,
    ctrl_right: bool,
    ctrl_generic: bool,
    shift_left: bool,
    shift_right: bool,
    shift_generic: bool,
    alt_left: bool,
    alt_right: bool,
    alt_generic: bool,
    win_left: bool,
    win_right: bool,
    gesture: WindowsGesture,
    generation: u64,
}
impl HookState {
    fn ctrl(&self) -> bool {
        self.ctrl_left || self.ctrl_right || self.ctrl_generic
    }
    fn shift(&self) -> bool {
        self.shift_left || self.shift_right || self.shift_generic
    }
    fn alt(&self) -> bool {
        self.alt_left || self.alt_right || self.alt_generic
    }
    fn windows(&self) -> bool {
        self.win_left || self.win_right
    }
}
static HOOK: OnceLock<Mutex<HookState>> = OnceLock::new();
static HOOK_CLAIMED: AtomicBool = AtomicBool::new(false);

struct HookClaim(&'static AtomicBool);
impl HookClaim {
    fn acquire() -> io::Result<Self> {
        Self::acquire_from(&HOOK_CLAIMED)
    }
    fn acquire_from(flag: &'static AtomicBool) -> io::Result<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| io::ErrorKind::AlreadyExists)?;
        Ok(Self(flag))
    }
}
impl Drop for HookClaim {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn state() -> &'static Mutex<HookState> {
    HOOK.get_or_init(|| Mutex::new(HookState::default()))
}

fn publish_hook_state(
    choice: ShortcutChoice,
    events: Sender<InputEvent>,
    cancelled: &AtomicBool,
) -> io::Result<bool> {
    let mut guard = state().lock().map_err(|_| io::ErrorKind::Other)?;
    if cancelled.load(Ordering::Acquire) {
        return Ok(false);
    }
    *guard = HookState {
        tx: Some(events),
        choice,
        enabled: true,
        ..HookState::default()
    };
    Ok(true)
}

pub struct WindowsInput {
    // A Google cleanup worker may keep this object after its resident host
    // exits. No second Local/Google hook may overwrite the global sender.
    claim: Option<HookClaim>,
    thread_id: Arc<AtomicU32>,
    thread: Option<thread::JoinHandle<()>>,
    receiver: Receiver<InputEvent>,
}
impl WindowsInput {
    pub fn start(choice: ShortcutChoice) -> io::Result<Self> {
        let claim = HookClaim::acquire()?;
        let (events, receiver) = mpsc::channel();
        let (ready, ack) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let thread_cancelled = cancelled.clone();
        let thread_id = Arc::new(AtomicU32::new(0));
        let thread_identity = thread_id.clone();
        let thread = thread::Builder::new()
            .name("voicetype-win-hotkey".into())
            .spawn(move || {
                let thread_id = unsafe { GetCurrentThreadId() };
                thread_identity.store(thread_id, Ordering::Release);
                let mut msg: MSG = unsafe { std::mem::zeroed() };
                unsafe {
                    PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
                }
                let hook = unsafe {
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), std::ptr::null_mut(), 0)
                };
                if hook.is_null() {
                    let _ = ready.send(Err(io::Error::last_os_error()));
                    return;
                }
                match publish_hook_state(choice, events, &thread_cancelled) {
                    Ok(true) => {}
                    Ok(false) => {
                        unsafe { UnhookWindowsHookEx(hook) };
                        let _ = ready.send(Err(io::ErrorKind::TimedOut.into()));
                        return;
                    }
                    Err(error) => {
                        unsafe { UnhookWindowsHookEx(hook) };
                        let _ = ready.send(Err(error));
                        return;
                    }
                }
                let _ = ready.send(Ok(thread_id));
                while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {}
                if let Ok(mut guard) = state().lock() {
                    *guard = HookState::default();
                }
                unsafe {
                    UnhookWindowsHookEx(hook);
                }
            })?;
        let mut owner = Self {
            claim: Some(claim),
            thread_id,
            thread: Some(thread),
            receiver,
        };
        match ack.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(_)) => Ok(owner),
            Ok(Err(error)) => {
                let _ = owner.stop();
                Err(error)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                cancelled.store(true, Ordering::Release);
                owner.disable();
                let _ = owner.stop();
                Err(io::ErrorKind::TimedOut.into())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = owner.stop();
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
    }
    pub fn poll(&self) -> Option<InputEvent> {
        self.receiver.try_recv().ok()
    }
    pub fn set_session_active(&self, active: bool) {
        if let Ok(mut guard) = state().lock() {
            guard.gesture.observe_owner(active);
        }
    }
    pub fn reject_start(&self) {
        if let Ok(mut guard) = state().lock() {
            guard.gesture.reject_start();
        }
    }
    /// Stop intercepting immediately if publishing the requested binding fails.
    /// The owning object still retains the hook claim until thread exit.
    pub fn disable(&self) {
        if let Ok(mut guard) = state().lock() {
            guard.enabled = false;
            guard.tx = None;
            guard.gesture = WindowsGesture::default();
        }
    }
    pub fn stop(&mut self) -> io::Result<()> {
        self.stop_with(
            |id| {
                if unsafe { PostThreadMessageW(id, WM_QUIT, 0, 0) } == 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            },
            Duration::from_millis(250),
        )
    }

    fn stop_with(
        &mut self,
        mut post_quit: impl FnMut(u32) -> io::Result<()>,
        budget: Duration,
    ) -> io::Result<()> {
        let Some(thread) = self.thread.as_ref() else {
            return Ok(());
        };
        if !thread.is_finished() {
            let id = self.thread_id.load(Ordering::Acquire);
            if id == 0 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            post_quit(id)?;
            let until = Instant::now() + budget;
            while !thread.is_finished() && Instant::now() < until {
                thread::sleep(Duration::from_millis(5));
            }
            if !thread.is_finished() {
                return Err(io::ErrorKind::TimedOut.into());
            }
        }
        let result: io::Result<()> = self
            .thread
            .take()
            .unwrap()
            .join()
            .map_err(|_| io::ErrorKind::BrokenPipe.into());
        self.claim.take();
        result
    }
}
impl Drop for WindowsInput {
    fn drop(&mut self) {
        if self.stop().is_ok() {
            return;
        }
        let Some(claim) = self.claim.take() else {
            return;
        };
        let Some(thread) = self.thread.take() else {
            return;
        };
        let owner = Arc::new(Mutex::new(Some((claim, thread))));
        let retry = owner.clone();
        let identity = self.thread_id.clone();
        let spawned = thread::Builder::new()
            .name("voicetype-win-hotkey-cleanup".into())
            .spawn(move || {
                let (claim, thread) = retry.lock().unwrap().take().unwrap();
                while !thread.is_finished() {
                    let id = identity.load(Ordering::Acquire);
                    if id != 0 {
                        unsafe { PostThreadMessageW(id, WM_QUIT, 0, 0) };
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                let _ = thread.join();
                drop(claim);
            });
        if spawned.is_err() {
            // Preserve the exclusive hook claim if a cleanup thread cannot be
            // made. Requiring restart is safer than starting a second hook.
            std::mem::forget(owner);
        }
    }
}

/// Whitelist a classic editable HWND. Browser/WinUI controls need TSF and stay
/// disabled until a text-service lease can reject same-HWND field changes.
fn focused_edit() -> Option<TargetLease> {
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
    let mut class = [0u16; 64];
    let length = unsafe { GetClassNameW(gui.hwndFocus, class.as_mut_ptr(), class.len() as i32) };
    if length <= 0 || String::from_utf16_lossy(&class[..length as usize]) != "Edit" {
        return None;
    }
    let style = unsafe { GetWindowLongW(gui.hwndFocus, GWL_STYLE) };
    if style & 0x20 != 0 {
        return None;
    } // ES_PASSWORD
    Some(TargetLease::new(
        format!(
            "win32-edit:{:x}:{:x}",
            window as usize, gui.hwndFocus as usize
        ),
        0,
    ))
}

pub fn focus_matches(target: &TargetLease) -> bool {
    focused_edit().is_some_and(|now| now.context_id == target.context_id)
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    }
    let event = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
    if event.flags & LLKHF_INJECTED != 0 {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    }
    let down = wparam as u32 == WM_KEYDOWN || wparam as u32 == WM_SYSKEYDOWN;
    let up = wparam as u32 == WM_KEYUP || wparam as u32 == WM_SYSKEYUP;
    if !down && !up {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    }
    let Ok(mut state) = state().lock() else {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    };
    if !state.enabled {
        drop(state);
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    }
    let vk = event.vkCode as u16;
    if vk == VK_CONTROL {
        state.ctrl_generic = down;
    }
    if vk == VK_LCONTROL {
        state.ctrl_left = down;
    }
    if vk == VK_RCONTROL {
        state.ctrl_right = down;
    }
    if vk == VK_SHIFT {
        state.shift_generic = down;
    }
    if vk == VK_LSHIFT {
        state.shift_left = down;
    }
    if vk == VK_RSHIFT {
        state.shift_right = down;
    }
    if vk == VK_MENU {
        state.alt_generic = down;
    }
    if vk == VK_LMENU {
        state.alt_left = down;
    }
    if vk == VK_RMENU {
        state.alt_right = down;
    }
    if vk == VK_LWIN {
        state.win_left = down;
    }
    if vk == VK_RWIN {
        state.win_right = down;
    }
    if vk == VK_ESCAPE {
        let outcome = state.gesture.escape(down);
        if let Some(GestureAction::Cancel) = outcome.action {
            if let Some(tx) = &state.tx {
                let _ = tx.send(InputEvent::Cancel);
            }
        }
        if outcome.suppress {
            return 1;
        }
    }
    let trigger_key = match state.choice {
        ShortcutChoice::Default => VK_CAPITAL,
        ShortcutChoice::CtrlShiftF9 => VK_F9,
    };
    if vk == trigger_key {
        let matched = shortcut_matches(
            state.choice,
            state.ctrl(),
            state.shift(),
            state.alt(),
            state.windows(),
        );
        let target = if down && matched && state.gesture.needs_target() {
            focused_edit().and_then(|mut target| {
                state.generation = state.generation.wrapping_add(1);
                if state.generation == 0 {
                    return None;
                }
                target.generation = state.generation;
                Some(target)
            })
        } else {
            None
        };
        let outcome = state.gesture.trigger(down, matched, || target);
        if let Some(action) = outcome.action {
            let event = match action {
                GestureAction::Start(target) => InputEvent::Start(target),
                GestureAction::Stop => InputEvent::Stop,
                GestureAction::Cancel => InputEvent::Cancel,
            };
            let sent = state.tx.as_ref().is_some_and(|tx| tx.send(event).is_ok());
            if !sent {
                state.gesture.reject_start();
            }
        }
        if outcome.suppress {
            return 1;
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    static TEST_CLAIM: AtomicBool = AtomicBool::new(false);
    static TEST_FAILED_POST: AtomicBool = AtomicBool::new(false);
    static TEST_DELAYED_EXIT: AtomicBool = AtomicBool::new(false);
    static TEST_LATE_START: AtomicBool = AtomicBool::new(false);
    static TEST_ACK_CANCEL: AtomicBool = AtomicBool::new(false);

    fn held_test_input(flag: &'static AtomicBool) -> (WindowsInput, Sender<()>) {
        let claim = HookClaim::acquire_from(flag).unwrap();
        let (release, held) = mpsc::channel();
        let thread = thread::spawn(move || {
            let _ = held.recv();
        });
        let (_, receiver) = mpsc::channel();
        (
            WindowsInput {
                claim: Some(claim),
                thread_id: Arc::new(AtomicU32::new(123)),
                thread: Some(thread),
                receiver,
            },
            release,
        )
    }

    #[test]
    fn failed_quit_post_keeps_hook_owner_until_observed_exit() {
        let (mut input, release) = held_test_input(&TEST_FAILED_POST);
        let error = input
            .stop_with(
                |_| Err(io::ErrorKind::BrokenPipe.into()),
                Duration::from_millis(20),
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(
            HookClaim::acquire_from(&TEST_FAILED_POST)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        release.send(()).unwrap();
        input.stop_with(|_| Ok(()), Duration::from_secs(1)).unwrap();
        assert!(HookClaim::acquire_from(&TEST_FAILED_POST).is_ok());
    }

    #[test]
    fn delayed_hook_exit_is_bounded_and_claim_stays_owned() {
        let (mut input, release) = held_test_input(&TEST_DELAYED_EXIT);
        let error = input
            .stop_with(|_| Ok(()), Duration::from_millis(20))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            HookClaim::acquire_from(&TEST_DELAYED_EXIT)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        release.send(()).unwrap();
        input.stop_with(|_| Ok(()), Duration::from_secs(1)).unwrap();
        assert!(HookClaim::acquire_from(&TEST_DELAYED_EXIT).is_ok());
    }

    #[test]
    fn late_hook_start_after_ack_timeout_keeps_claim_until_thread_exit() {
        let (input, release) = held_test_input(&TEST_LATE_START);
        input.thread_id.store(0, Ordering::Release);
        // An ack timeout drops the start owner while its thread has not yet
        // published a thread id. The fallback owns the claim and retries.
        drop(input);
        assert_eq!(
            HookClaim::acquire_from(&TEST_LATE_START)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while TEST_LATE_START.load(Ordering::Acquire) && Instant::now() < until {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!TEST_LATE_START.load(Ordering::Acquire));
    }

    #[test]
    fn timed_out_start_cannot_publish_a_late_hook_sender() {
        let cancelled = AtomicBool::new(true);
        let (tx, _) = mpsc::channel();
        assert!(!publish_hook_state(ShortcutChoice::CtrlShiftF9, tx, &cancelled).unwrap());
        assert!(!state().lock().unwrap().enabled);

        let (mut owner, release) = held_test_input(&TEST_ACK_CANCEL);
        let (tx, _) = mpsc::channel();
        cancelled.store(false, Ordering::Release);
        assert!(publish_hook_state(ShortcutChoice::CtrlShiftF9, tx, &cancelled).unwrap());
        cancelled.store(true, Ordering::Release);
        owner.disable();
        assert!(!state().lock().unwrap().enabled);
        release.send(()).unwrap();
        owner.stop_with(|_| Ok(()), Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn detached_owner_after_provider_spawn_failure_rejects_a_second_hook() {
        let host = Arc::new(HookClaim::acquire_from(&TEST_CLAIM).unwrap());
        // The provider factory fails after moving its input Arc into a cleanup
        // owner. Dropping the host must not release the claim prematurely.
        let cleanup_owner = host.clone();
        drop(host);
        assert_eq!(
            HookClaim::acquire_from(&TEST_CLAIM).err().unwrap().kind(),
            io::ErrorKind::AlreadyExists
        );
        drop(cleanup_owner);
        let replacement = HookClaim::acquire_from(&TEST_CLAIM).unwrap();
        drop(replacement);
    }
}
