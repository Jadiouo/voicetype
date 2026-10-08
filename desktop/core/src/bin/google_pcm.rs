//! Linux-only, exact-argv `pw-record` compatibility child for the official
//! CLI. The native app owns the microphone; this child only relays bounded PCM
//! into the official recorder pipe and acknowledges when its pipe is empty.
#[cfg(target_os = "linux")]
fn main() {
    if run().is_err() {
        eprintln!("VoiceType PCM relay failed");
        std::process::exit(74);
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    std::process::exit(69);
}

#[cfg(target_os = "linux")]
fn run() -> std::io::Result<()> {
    use std::{
        env,
        io::{self, Read, Write},
        os::fd::AsRawFd,
        os::unix::net::UnixStream,
        path::PathBuf,
        thread,
        time::{Duration, Instant},
    };
    let expected = ["--rate=16000", "--channels=1", "--format=s16", "--raw", "-"];
    if env::args().skip(1).collect::<Vec<_>>() != expected {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let path =
        PathBuf::from(env::var_os("VOICETYPE_GOOGLE_RELAY").ok_or(io::ErrorKind::InvalidInput)?);
    if !path.is_absolute() || path.as_os_str().len() >= 108 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let nonce = env::var("VOICETYPE_GOOGLE_NONCE").map_err(|_| io::ErrorKind::InvalidInput)?;
    if nonce.len() != 64 || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut stream = UnixStream::connect(path)?;
    stream.write_all(nonce.as_bytes())?;
    let mut acceptance = [0u8; 2];
    stream.read_exact(&mut acceptance)?;
    if acceptance != *b"OK" {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut stdout = io::stdout().lock();
    loop {
        stream.write_all(&640u32.to_be_bytes())?;
        let mut size = [0u8; 4];
        stream.read_exact(&mut size)?;
        let size = u32::from_be_bytes(size) as usize;
        if size > 640 || size % 2 != 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if size == 0 {
            let until = Instant::now() + Duration::from_secs(2);
            loop {
                let mut pending: libc::c_int = 0;
                if unsafe { libc::ioctl(stdout.as_raw_fd(), libc::FIONREAD, &mut pending) } < 0 {
                    return Err(io::Error::last_os_error());
                }
                if pending == 0 {
                    break;
                }
                if Instant::now() >= until {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                thread::sleep(Duration::from_millis(5));
            }
            stream.write_all(&0u32.to_be_bytes())?;
            break;
        }
        let mut frame = vec![0u8; size];
        stream.read_exact(&mut frame)?;
        stdout.write_all(&frame)?;
        stdout.flush()?;
        stream.write_all(&(size as u32).to_be_bytes())?;
    }
    // The CLI's F5 stop closes its recorder pipe. Keep the child owned until
    // then; an early exit could let a second F5 accept a partial result.
    loop {
        let mut fd = libc::pollfd {
            fd: stdout.as_raw_fd(),
            events: libc::POLLERR | libc::POLLHUP,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut fd, 1, 50) };
        if result > 0 && fd.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            break;
        }
        if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
