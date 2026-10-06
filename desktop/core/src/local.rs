//! Linux local-engine transport. Use on the native dispatcher, never the webview
//! event thread. The caller retains ownership of the child process and must reap
//! it before treating a broken connection as released.
use crate::{
    CommandRejected, Provider, ProviderCommand, ProviderEvent, ProviderPort, SessionFailure,
    SessionKey,
};
use serde_json::{json, Value};
use socket2::{Domain, SockAddr, Socket, Type};
use std::{
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::net::UnixStream},
    path::Path,
    time::{Duration, Instant},
};

pub(crate) enum CorrectionResult {
    Changed,
    Unchanged,
    Rejected,
}

pub struct LocalConnection {
    wire: Wire,
    active: Option<SessionKey>,
    faulted: bool,
    request: u64,
}

impl LocalConnection {
    /// Authenticate the actual Unix peer against the owned child PID and current
    /// user, then negotiate before any recording command can be accepted.
    pub fn connect_to_process(path: &Path, pid: u32, budget: Duration) -> io::Result<Self> {
        let deadline = deadline(budget);
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        socket.connect_timeout(&SockAddr::unix(path)?, remaining(deadline)?)?;
        let stream: UnixStream = socket.into();
        let mut wire = Wire::authenticated(stream, Some(pid))?;
        wire.send(&json!({"type":"desktop_status","request":1}), deadline)?;
        let reply = wire.read(deadline)?;
        let value = &reply["value"];
        let capabilities = value["capabilities"].as_array();
        if reply["type"] != "info"
            || value["desktop_protocol"] != 1
            || value["request"] != 1
            || !capabilities.is_some_and(|items| {
                items.contains(&json!("session_events")) && items.contains(&json!("suspend"))
            })
        {
            return Err(invalid("incompatible local engine protocol"));
        }
        if value["session_busy"] != false {
            return Err(io::Error::other("local engine is not idle"));
        }
        Ok(Self {
            wire,
            active: None,
            faulted: false,
            request: 1,
        })
    }

    /// A polling timeout is normal while recording. EOF/protocol errors are not
    /// release acknowledgements: the session stays owned until verified cleanup.
    pub fn poll_event(
        &mut self,
        budget: Duration,
    ) -> io::Result<Option<(SessionKey, ProviderEvent)>> {
        self.usable()?;
        let Some(key) = self.active else {
            return Ok(None);
        };
        let deadline = deadline(budget);
        loop {
            let reply = match self.wire.read(deadline) {
                Ok(value) => value,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Ok(None)
                }
                Err(e) => {
                    self.faulted = true;
                    return Err(e);
                }
            };
            let Some(session) = reply["session"].as_u64() else {
                self.faulted = true;
                return Err(invalid("engine event has no session"));
            };
            if session != key.id() {
                continue;
            }
            let event = match reply["type"].as_str() {
                Some("error") if reply["code"].is_string() && reply["text"].is_string() => {
                    ProviderEvent::Failed(SessionFailure::ProviderFailed)
                }
                Some("state") if reply["value"] == "recording" => ProviderEvent::Recording,
                Some("state") if reply["value"] == "idle" => {
                    self.active = None;
                    ProviderEvent::Released
                }
                Some("result") => match reply["text"].as_str() {
                    Some(text) => ProviderEvent::Final(text.into()),
                    None => {
                        self.faulted = true;
                        return Err(invalid("invalid final text"));
                    }
                },
                _ => {
                    self.faulted = true;
                    return Err(invalid("unexpected local engine event"));
                }
            };
            return Ok(Some((key, event)));
        }
    }

    /// Call after session release and before opening another provider's capture.
    /// Success means the owned daemon acknowledged dropping its warm stream.
    pub fn suspend(&mut self, budget: Duration) -> io::Result<()> {
        self.usable()?;
        if self.active.is_some() {
            return Err(io::Error::other("local session is still busy"));
        }
        self.request = self
            .request
            .checked_add(1)
            .ok_or_else(|| invalid("request IDs exhausted"))?;
        let deadline = deadline(budget);
        let result = (|| {
            self.wire.send(
                &json!({"type":"desktop_suspend","request":self.request}),
                deadline,
            )?;
            let reply = self.wire.read(deadline)?;
            if reply
                != json!({"type":"info","value":{"desktop_protocol":1,"request":self.request,"microphone":"closed"}})
            {
                return Err(invalid("microphone handoff was not acknowledged"));
            }
            Ok(())
        })();
        if result.is_err() {
            self.faulted = true;
        }
        result
    }

    /// Correct only an idle, previously acknowledged engine session. The daemon
    /// performs its existing attribution/expiry/terminology checks unchanged.
    pub(crate) fn correct(
        &mut self,
        key: SessionKey,
        message: &Value,
        budget: Duration,
    ) -> io::Result<CorrectionResult> {
        self.usable()?;
        if self.active.is_some() || key.provider() != Provider::Local {
            return Err(io::Error::other("local session is still busy"));
        }
        let until = deadline(budget);
        let result = (|| {
            let mut message = message.clone();
            message["session"] = json!(key.id());
            self.wire.send(&message, until)?;
            let reply = self.wire.read(until)?;
            if reply["type"] == "info" && reply["value"].is_boolean() {
                Ok(if reply["value"] == true {
                    CorrectionResult::Changed
                } else {
                    CorrectionResult::Unchanged
                })
            } else if reply["type"] == "error"
                && reply.get("session").is_none()
                && reply["code"].is_string()
                && reply["text"].is_string()
            {
                Ok(CorrectionResult::Rejected)
            } else {
                Err(invalid("unexpected correction response"))
            }
        })();
        if result.is_err() {
            self.faulted = true;
        }
        result
    }

    fn usable(&self) -> io::Result<()> {
        if self.faulted {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "local engine requires cleanup",
            ))
        } else {
            Ok(())
        }
    }
}

impl ProviderPort for LocalConnection {
    fn send(&mut self, command: ProviderCommand) -> Result<(), CommandRejected> {
        if self.faulted {
            return Err(CommandRejected);
        }
        let message = match command {
            ProviderCommand::Start {
                key,
                target,
                context,
            } if key.provider() == Provider::Local && self.active.is_none() => {
                self.active = Some(key);
                json!({"type":"start","session":key.id(),"context_id":target.context_id,
                    "program":context.program,"context_text":context.context_text,
                    "selected_text":context.selected_text,"session_events":true})
            }
            ProviderCommand::Stop { key } if self.active == Some(key) => {
                json!({"type":"stop","session":key.id()})
            }
            ProviderCommand::Cancel { key } if self.active == Some(key) => {
                json!({"type":"cancel","session":key.id()})
            }
            _ => return Err(CommandRejected),
        };
        if self
            .wire
            .send(&message, deadline(Duration::from_millis(300)))
            .is_err()
        {
            // A write may have reached the daemon partially or completely. Keep
            // ownership and let the dispatcher fail/clean up the owned process;
            // returning rejection would incorrectly allow an immediate restart.
            self.faulted = true;
        }
        Ok(())
    }
}

pub(crate) struct Wire {
    stream: UnixStream,
    buffer: Vec<u8>,
}
impl Wire {
    pub(crate) fn authenticated(stream: UnixStream, pid: Option<u32>) -> io::Result<Self> {
        let mut credentials = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: credentials and size are correctly sized writable values. The
        // connected fd is live for the entire kernel credential query.
        let status = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut size,
            )
        };
        if status != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: geteuid has no preconditions and does not mutate process state.
        let uid = unsafe { libc::geteuid() };
        if size as usize != std::mem::size_of::<libc::ucred>()
            || credentials.uid != uid
            || credentials.pid <= 0
            || pid.is_some_and(|pid| credentials.pid as u32 != pid)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unexpected engine process",
            ));
        }
        stream.set_nonblocking(false)?;
        Ok(Self {
            stream,
            buffer: Vec::new(),
        })
    }

    pub(crate) fn send(&mut self, value: &Value, until: Instant) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        let mut remaining_bytes = &bytes[..];
        while !remaining_bytes.is_empty() {
            self.stream.set_write_timeout(Some(remaining(until)?))?;
            let sent = self.stream.write(remaining_bytes)?;
            if sent == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            remaining_bytes = &remaining_bytes[sent..];
        }
        Ok(())
    }
    pub(crate) fn read(&mut self, until: Instant) -> io::Result<Value> {
        loop {
            let budget = remaining(until)?;
            if let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
                if end > 256 * 1024 {
                    return Err(invalid("engine frame too large"));
                }
                let reply = serde_json::from_slice(&self.buffer[..end])
                    .map_err(|_| invalid("invalid engine JSON"));
                self.buffer.drain(..=end);
                return reply;
            }
            if self.buffer.len() > 256 * 1024 {
                return Err(invalid("engine frame too large"));
            }
            self.stream.set_read_timeout(Some(budget))?;
            let mut bytes = [0; 4096];
            let count = self.stream.read(&mut bytes)?;
            if count == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            self.buffer.extend_from_slice(&bytes[..count]);
        }
    }
}

pub(crate) fn deadline(budget: Duration) -> Instant {
    Instant::now() + budget.min(Duration::from_secs(2))
}
pub(crate) fn remaining(until: Instant) -> io::Result<Duration> {
    until
        .checked_duration_since(Instant::now())
        .filter(|value| !value.is_zero())
        .ok_or_else(|| io::ErrorKind::TimedOut.into())
}
pub(crate) fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}
