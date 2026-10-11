//! Temporary recovery-only Ctrl+CapsLock toggle. This hook consumes the chord;
//! it must be replaced by TSF's preserved-key route before automatic delivery.
use crate::windows_gesture::{GestureAction, WindowsGesture};
use crate::TargetLease;
use std::{
    io,
    mem::size_of,
    sync::{
        mpsc::{self, Receiver, Sender},
        Mutex, OnceLock,
    },
    thread,
};
use windows_sys::Win32::{
    Foundation::{LPARAM, LRESULT, WPARAM},
    System::Threading::GetCurrentThreadId,
    UI::{
        Input::KeyboardAndMouse::{VK_CAPITAL, VK_CONTROL, VK_ESCAPE, VK_LCONTROL, VK_RCONTROL},
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
    ctrl_left: bool,
    ctrl_right: bool,
    ctrl_generic: bool,
    gesture: WindowsGesture,
    generation: u64,
}
impl HookState {
    fn ctrl(&self) -> bool {
        self.ctrl_left || self.ctrl_right || self.ctrl_generic
    }
}
static HOOK: OnceLock<Mutex<HookState>> = OnceLock::new();
fn state() -> &'static Mutex<HookState> {
    HOOK.get_or_init(|| Mutex::new(HookState::default()))
}

pub struct WindowsInput {
    thread_id: u32,
    thread: Option<thread::JoinHandle<()>>,
    receiver: Receiver<InputEvent>,
}
impl WindowsInput {
    pub fn start() -> io::Result<Self> {
        let (events, receiver) = mpsc::channel();
        let (ready, ack) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("voicetype-win-hotkey".into())
            .spawn(move || {
                let thread_id = unsafe { GetCurrentThreadId() };
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
                if let Ok(mut guard) = state().lock() {
                    guard.tx = Some(events);
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
        let thread_id = ack.recv().map_err(|_| io::ErrorKind::BrokenPipe)??;
        Ok(Self {
            thread_id,
            thread: Some(thread),
            receiver,
        })
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
    pub fn stop(&mut self) {
        unsafe {
            PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for WindowsInput {
    fn drop(&mut self) {
        self.stop();
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
    if vk == VK_CAPITAL {
        let ctrl = state.ctrl();
        let target = if down && ctrl && state.gesture.needs_target() {
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
        let outcome = state.gesture.caps(down, ctrl, || target);
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
