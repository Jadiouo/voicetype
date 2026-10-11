//! Fixed, bounded Linux witnesses for an explicit legacy Google handoff.
//! No constructor or read path stops a service; `stop` is called only by the
//! separate transfer transaction after the user chooses that action.
use super::legacy_transfer::{FcitxIdentity, LegacyService, UnitIdentity};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs, io,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const UNIT: &str = "googlevoiceinput.service";

pub struct SystemdLegacyService {
    listener: PathBuf,
}

impl SystemdLegacyService {
    pub fn new(runtime: &Path) -> io::Result<Self> {
        if !runtime.is_absolute() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Self {
            listener: runtime.join("googlevoiceinput/ipc.sock"),
        })
    }

    fn properties(&self) -> io::Result<HashMap<String, String>> {
        let output = bounded(
            "systemctl",
            &[
                "--user",
                "show",
                UNIT,
                "-p",
                "LoadState",
                "-p",
                "ActiveState",
                "-p",
                "UnitFileState",
                "-p",
                "FragmentPath",
                "-p",
                "ExecStart",
                "-p",
                "MainPID",
                "-p",
                "ControlGroup",
            ],
            Duration::from_secs(3),
        )?;
        let fields = parse_properties(&output)?;
        if fields.get("LoadState").map(String::as_str) != Some("loaded") {
            return Err(io::ErrorKind::NotFound.into());
        }
        Ok(fields)
    }
}

impl LegacyService for SystemdLegacyService {
    fn fcitx_identity(&self) -> io::Result<FcitxIdentity> {
        fcitx_identity()
    }

    fn snapshot(&self) -> io::Result<UnitIdentity> {
        let fields = self.properties()?;
        // `ExecStart` from `systemctl show` includes last-run PID/times/status.
        // `cat` is the bounded static fragment plus every effective drop-in.
        let definition = bounded(
            "systemctl",
            &["--user", "cat", UNIT],
            Duration::from_secs(3),
        )?;
        snapshot_from(&fields, &definition)
    }

    fn stop(&self) -> io::Result<()> {
        bounded(
            "systemctl",
            &["--user", "stop", UNIT],
            Duration::from_secs(20),
        )
        .map(|_| ())
    }

    fn quiescent(&self, original_cgroup: Option<&Path>) -> io::Result<bool> {
        let fields = self.properties()?;
        if value(&fields, "ActiveState")? != "inactive" || value(&fields, "MainPID")? != "0" {
            return Ok(false);
        }
        if let Some(cgroup) = original_cgroup {
            if !cgroup.is_absolute() || cgroup.components().any(|part| part.as_os_str() == "..") {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let full = Path::new("/sys/fs/cgroup").join(
                cgroup
                    .strip_prefix("/")
                    .map_err(|_| io::ErrorKind::InvalidData)?,
            );
            match fs::read(full.join("cgroup.procs")) {
                Ok(procs) if !procs.iter().all(u8::is_ascii_whitespace) => return Ok(false),
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        match UnixStream::connect(&self.listener) {
            Ok(_) => Ok(false),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                Ok(true)
            }
            Err(e) => Err(e),
        }
    }

    fn start(&self) -> io::Result<()> {
        bounded(
            "systemctl",
            &["--user", "start", UNIT],
            Duration::from_secs(20),
        )?;
        if self.snapshot()?.active {
            Ok(())
        } else {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
}

fn snapshot_from(fields: &HashMap<String, String>, definition: &str) -> io::Result<UnitIdentity> {
    if definition.is_empty() || definition.len() > 16 * 1024 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let fragment = PathBuf::from(value(&fields, "FragmentPath")?);
    if !fragment.is_absolute() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    if value(&fields, "ExecStart")?.is_empty() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let state = value(&fields, "UnitFileState")?;
    if !matches!(state, "enabled" | "enabled-runtime" | "disabled") {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let active = value(&fields, "ActiveState")?;
    if !matches!(active, "active" | "inactive") {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let group = value(&fields, "ControlGroup")?;
    let cgroup = if group.is_empty() {
        None
    } else {
        if !Path::new(group).is_absolute()
            || Path::new(group)
                .components()
                .any(|part| part.as_os_str() == "..")
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Some(PathBuf::from(group))
    };
    if active == "active" && cgroup.is_none() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(UnitIdentity {
        fragment,
        unit_definition_sha256: format!("{:x}", Sha256::digest(definition.as_bytes())),
        unit_file_state: state.to_owned(),
        enabled: state.starts_with("enabled"),
        active: active == "active",
        cgroup,
    })
}

/// The Fcitx D-Bus name supplies one process. PID alone is not an identity;
/// `/proc/<pid>/stat` field 22 disambiguates reuse across reloads.
pub fn fcitx_identity() -> io::Result<FcitxIdentity> {
    let owner = bounded(
        "busctl",
        &[
            "--user",
            "call",
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetNameOwner",
            "s",
            "org.fcitx.Fcitx5",
        ],
        Duration::from_secs(2),
    )?;
    let owner = owner
        .trim()
        .strip_prefix("s \"")
        .and_then(|s| s.strip_suffix('"'))
        .filter(|s| s.starts_with(':') && s.len() < 64)
        .ok_or(io::ErrorKind::InvalidData)?;
    let pid = bounded(
        "busctl",
        &[
            "--user",
            "call",
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetConnectionUnixProcessID",
            "s",
            owner,
        ],
        Duration::from_secs(2),
    )?;
    let pid: u32 = pid
        .trim()
        .strip_prefix("u ")
        .ok_or(io::ErrorKind::InvalidData)?
        .parse()
        .map_err(|_| io::ErrorKind::InvalidData)?;
    if pid == 0 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let tail = stat.rsplit_once(") ").ok_or(io::ErrorKind::InvalidData)?.1;
    let start_ticks = tail
        .split_whitespace()
        .nth(19)
        .ok_or(io::ErrorKind::InvalidData)?
        .parse()
        .map_err(|_| io::ErrorKind::InvalidData)?;
    Ok(FcitxIdentity { pid, start_ticks })
}

fn parse_properties(output: &str) -> io::Result<HashMap<String, String>> {
    let mut fields = HashMap::new();
    for line in output.lines() {
        let (key, value) = line.split_once('=').ok_or(io::ErrorKind::InvalidData)?;
        if fields.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(io::ErrorKind::InvalidData.into());
        }
    }
    Ok(fields)
}
fn value<'a>(fields: &'a HashMap<String, String>, key: &str) -> io::Result<&'a str> {
    fields
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| io::ErrorKind::InvalidData.into())
}
fn bounded(program: &str, args: &[&str], budget: Duration) -> io::Result<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + budget;
    loop {
        if let Some(status) = child.try_wait()? {
            let output = child.wait_with_output()?;
            if !status.success() || output.stdout.len() > 16 * 1024 {
                return Err(io::ErrorKind::InvalidData.into());
            }
            return String::from_utf8(output.stdout).map_err(|_| io::ErrorKind::InvalidData.into());
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Err(io::ErrorKind::TimedOut.into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unit_identity_ignores_process_times_but_detects_static_unit_change() {
        let mut fields = HashMap::from([
            ("FragmentPath".to_owned(), "/fixture/googlevoiceinput.service".to_owned()),
            ("ExecStart".to_owned(), "{ path=/usr/bin/python3 ; argv[]=/usr/bin/python3 bridge.py ; start_time=[n/a] ; pid=0 ; status=0/0 }".to_owned()),
            ("UnitFileState".to_owned(), "enabled".to_owned()),
            ("ActiveState".to_owned(), "active".to_owned()),
            ("ControlGroup".to_owned(), "/user.slice/googlevoiceinput.service".to_owned()),
        ]);
        let original = snapshot_from(&fields, "# /fixture/googlevoiceinput.service\n[Service]\nExecStart=/usr/bin/python3 bridge.py\n").unwrap();
        fields.insert("ExecStart".into(), "{ path=/usr/bin/python3 ; argv[]=/usr/bin/python3 bridge.py ; start_time=Sat 2026-10-11 ; pid=123 ; status=0/0 }".into());
        fields.insert("ActiveState".into(), "inactive".into());
        let stopped = snapshot_from(&fields, "# /fixture/googlevoiceinput.service\n[Service]\nExecStart=/usr/bin/python3 bridge.py\n").unwrap();
        assert_eq!(
            original.unit_definition_sha256,
            stopped.unit_definition_sha256
        );
        let edited = snapshot_from(
            &fields,
            "# /fixture/googlevoiceinput.service\n[Service]\nExecStart=/usr/bin/python3 other.py\n",
        )
        .unwrap();
        assert_ne!(
            original.unit_definition_sha256,
            edited.unit_definition_sha256
        );
    }
    #[test]
    fn duplicate_or_malformed_unit_properties_fail_closed() {
        assert!(parse_properties("ActiveState=active\nActiveState=inactive\n").is_err());
        assert!(parse_properties("ActiveState\n").is_err());
        assert_eq!(
            parse_properties("ActiveState=inactive\nMainPID=0\n").unwrap()["MainPID"],
            "0"
        );
    }
    #[test]
    fn bounded_child_is_reaped_and_output_is_captured() {
        assert_eq!(
            bounded("printf", &["ready"], Duration::from_secs(1)).unwrap(),
            "ready"
        );
    }
}
