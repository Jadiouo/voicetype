use socket2::{Domain, SockAddr, Socket, Type};
use std::{
    io::{self, Read, Write},
    path::Path,
    time::{Duration, Instant},
};

pub(super) fn ping(path: &Path, budget: Duration) -> io::Result<()> {
    let deadline = Instant::now() + budget.min(Duration::from_secs(2));
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|v| !v.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "engine status timed out"))
    };
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    socket.connect_timeout(&SockAddr::unix(path)?, remaining()?)?;
    let mut stream: std::os::unix::net::UnixStream = socket.into();
    stream.set_write_timeout(Some(remaining()?))?;
    stream.write_all(b"{\"type\":\"ping\"}\n")?;
    let mut data = Vec::new();
    loop {
        stream.set_read_timeout(Some(remaining()?))?;
        let mut buffer = [0; 512];
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "engine disconnected",
            ));
        }
        data.extend_from_slice(&buffer[..count]);
        if data.len() > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "engine status too large",
            ));
        }
        if let Some(end) = data.iter().position(|b| *b == b'\n') {
            let reply: serde_json::Value = serde_json::from_slice(&data[..end])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            return if reply == serde_json::json!({"type":"pong"}) {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected engine status",
                ))
            };
        }
    }
}
