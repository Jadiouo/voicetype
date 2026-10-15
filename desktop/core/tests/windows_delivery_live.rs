//! Real SendInput injection into real Win32 controls. First probes whether the
//! session has an interactive desktop where this process can own the foreground;
//! when it cannot, every case is reported as SKIPPED with the reason (never as a
//! silent pass). Run with `--nocapture` to see the `PROBE` lines.
#![cfg(windows)]

use std::{ptr::null, ptr::null_mut, thread, time::Duration};
use voicetype_app_core::windows_delivery::{capture_target_including_own_process, Win32Delivery};
use voicetype_app_core::{DeliveryOutcome, DeliveryPort};
use windows_sys::Win32::{
    Foundation::HWND,
    System::LibraryLoader::LoadLibraryW,
    UI::{
        Input::KeyboardAndMouse::{keybd_event, SetFocus, KEYEVENTF_KEYUP, VK_MENU},
        WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, DispatchMessageW, GetForegroundWindow, GetWindowTextW,
            PeekMessageW, SetForegroundWindow, ShowWindow, TranslateMessage, MSG, PM_REMOVE,
            SW_SHOW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
        },
    },
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn pump(ms: u64) {
    let end = std::time::Instant::now() + Duration::from_millis(ms);
    while std::time::Instant::now() < end {
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        while unsafe { PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) } != 0 {
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn make(class: &str, x: i32) -> HWND {
    let class = wide(class);
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            wide("").as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE | 0x0004, /* ES_MULTILINE */
            x,
            100,
            400,
            200,
            null_mut(),
            null_mut(),
            null_mut(),
            null(),
        )
    };
    if !hwnd.is_null() {
        unsafe { ShowWindow(hwnd, SW_SHOW) };
    }
    hwnd
}

fn text(hwnd: HWND) -> String {
    let mut buf = vec![0u16; 8192];
    let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn foreground(hwnd: HWND) -> bool {
    for attempt in 0..5 {
        unsafe {
            if attempt > 0 {
                // Pressing Alt lets a background process take the foreground.
                keybd_event(VK_MENU as u8, 0, 0, 0);
                keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);
            }
            SetForegroundWindow(hwnd);
            SetFocus(hwnd);
        }
        pump(150);
        if unsafe { GetForegroundWindow() } == hwnd {
            return true;
        }
    }
    false
}

/// With VOICETYPE_REQUIRE_INTERACTIVE=1 a missing interactive desktop fails the test.
fn skip(reason: &str) {
    println!("PROBE SKIPPED reason={reason}");
    assert!(
        std::env::var("VOICETYPE_REQUIRE_INTERACTIVE").as_deref() != Ok("1"),
        "interactive desktop required but unavailable: {reason}"
    );
}

fn run_case(class: &str) {
    if class.starts_with("RICHEDIT") {
        let lib = wide("msftedit.dll");
        if unsafe { LoadLibraryW(lib.as_ptr()) }.is_null() {
            skip(&format!("{class}: msftedit.dll not loadable"));
            return;
        }
    }
    let a = make(class, 50);
    if a.is_null() {
        skip(&format!("{class}: CreateWindowExW failed"));
        return;
    }
    if !foreground(a) {
        skip(&format!(
            "{class} interactive=false: could not become foreground window"
        ));
        unsafe { DestroyWindow(a) };
        return;
    }
    println!("PROBE case={class} interactive=true");
    let lease =
        capture_target_including_own_process().expect("foreground control must yield a lease");
    let mut port = Win32Delivery::allowing_own_process();
    let sample = "你好 hello 😀\nsecond";
    let outcome = port.commit_if_focused(&lease, sample);
    pump(500);
    let got = text(a);
    println!(
        "PROBE case={class} outcome={outcome:?} typed_len={}",
        got.encode_utf16().count()
    );
    assert_eq!(outcome, DeliveryOutcome::Delivered);
    assert_eq!(got.replace("\r\n", "\n"), "你好 hello 😀 second");

    // Focus moves to a second window before delivery: nothing may be typed.
    let b = make(class, 500);
    assert!(!b.is_null());
    if foreground(b) {
        let before_a = text(a);
        let outcome = port.commit_if_focused(&lease, "should not appear");
        pump(300);
        println!("PROBE case={class} focus_change outcome={outcome:?}");
        assert_eq!(outcome, DeliveryOutcome::FocusChanged);
        assert_eq!(text(a), before_a);
        assert_eq!(text(b), "");
    } else {
        skip(&format!(
            "{class} focus_change: second window not foreground"
        ));
    }
    unsafe {
        DestroyWindow(b);
        DestroyWindow(a);
    }
}

#[test]
fn real_sendinput_into_edit_and_richedit() {
    run_case("EDIT");
    run_case("RICHEDIT50W");
}
