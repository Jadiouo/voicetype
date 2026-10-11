//! CI-only external process for ConPTY controls and editor callback. It never
//! records or connects to an account. Release installers do not bundle it.
#[cfg(windows)]
fn main() {
    if run().is_err() {
        std::process::exit(2);
    }
}

#[cfg(not(windows))]
fn main() {
    std::process::exit(69);
}

#[cfg(windows)]
fn run() -> std::io::Result<()> {
    use std::{
        env, fs,
        io::{self, Read},
        process::Command,
    };
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT,
        ENABLE_PROCESSED_INPUT, STD_INPUT_HANDLE,
    };
    let log = env::args_os()
        .skip_while(|arg| arg != "--log-file")
        .nth(1)
        .ok_or(io::ErrorKind::InvalidInput)?;
    fs::write(log, "")?;
    let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let mut mode = 0;
    if unsafe { GetConsoleMode(input, &mut mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe {
        SetConsoleMode(
            input,
            mode & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    fs::write(env::current_dir()?.join("ready"), "")?;
    let temporary = env::var_os("VOICETYPE_GOOGLE_TMP").ok_or(io::ErrorKind::InvalidInput)?;
    let editor = env::var("EDITOR").map_err(|_| io::ErrorKind::InvalidInput)?;
    let editor = editor.trim_matches('"');
    let mut controls = Vec::new();
    let mut edits = 0usize;
    let mut stdin = io::stdin().lock();
    loop {
        let mut byte = [0u8; 1];
        stdin.read_exact(&mut byte)?;
        controls.push(byte[0]);
        if byte[0] == 7 {
            let draft = std::path::PathBuf::from(&temporary).join("prompt.txt");
            let text = ["", "早期", "完整句子"][edits.min(2)];
            edits += 1;
            fs::write(&draft, text)?;
            if !Command::new(editor).arg(&draft).status()?.success() {
                return Err(io::ErrorKind::Other.into());
            }
        }
        if controls.ends_with(b"\x1b[15~") {
            fs::write(env::current_dir()?.join("controls"), &controls)?;
        }
    }
}
