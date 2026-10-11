//! Temporary Fcitx routing. Legacy configuration and services are never changed.
//! The record is published before handoff and names this live owner. Removing it
//! (or losing its socket) rolls back to Fcitx's unchanged legacy endpoint.
use crate::ShortcutChoice;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

pub(crate) struct InputLease {
    record: PathBuf,
    socket: String,
    google_toggle: bool,
    _lock: File,
}

impl InputLease {
    pub fn acquire(runtime: &Path, socket: &Path, choice: ShortcutChoice) -> io::Result<Self> {
        Self::acquire_mode(runtime, socket, false, choice)
    }

    pub fn acquire_google(
        runtime: &Path,
        socket: &Path,
        choice: ShortcutChoice,
    ) -> io::Result<Self> {
        Self::acquire_mode(runtime, socket, true, choice)
    }

    fn acquire_mode(
        runtime: &Path,
        socket: &Path,
        google_toggle: bool,
        choice: ShortcutChoice,
    ) -> io::Result<Self> {
        private_directory(runtime)?;
        let directory = runtime.join("voicetype-app-input");
        match fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        private_directory(&directory)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(directory.join("lock"))?;
        let meta = lock.metadata()?;
        if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let socket = socket
            .to_str()
            .filter(|s| s.starts_with('/') && s.len() < 108 && !s.contains(['\n', '\r', '\0']))
            .ok_or(io::ErrorKind::InvalidInput)?
            .to_owned();
        let record = directory.join("owner");
        Self::publish(&record, &socket, google_toggle, choice)?;
        Ok(Self {
            record,
            socket,
            google_toggle,
            _lock: lock,
        })
    }

    /// Atomically replace only this owner's record while retaining the same
    /// exclusive lock. Used solely before any recording/session is accepted.
    pub fn rebind(&self, choice: ShortcutChoice) -> io::Result<()> {
        Self::publish(&self.record, &self.socket, self.google_toggle, choice)
    }

    fn publish(
        record: &Path,
        socket: &str,
        google_toggle: bool,
        choice: ShortcutChoice,
    ) -> io::Result<()> {
        let mut staged = tempfile::NamedTempFile::new_in(
            record.parent().expect("owner record has a directory"),
        )?;
        if choice == ShortcutChoice::CtrlShiftF9 {
            let mode = if google_toggle {
                "google-toggle-ctrl-shift-f9"
            } else {
                "local-hold-ctrl-shift-f9"
            };
            writeln!(
                staged,
                "voicetype-input-v3\n{}\n{socket}\n{mode}",
                std::process::id()
            )?;
        } else if google_toggle {
            writeln!(
                staged,
                "voicetype-input-v2\n{}\n{}\ngoogle-toggle",
                std::process::id(),
                socket
            )?;
        } else {
            writeln!(
                staged,
                "voicetype-input-v1\n{}\n{}",
                std::process::id(),
                socket
            )?;
        }
        staged.as_file().sync_all()?;
        staged.persist(record).map_err(|e| e.error)?;
        Ok(())
    }
}

fn private_directory(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !path.is_absolute()
        || !meta.is_dir()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}

impl Drop for InputLease {
    fn drop(&mut self) {
        // Still holding the lock: a new owner cannot publish before this removal.
        // If removal fails, socket closure still makes the addon fall back.
        let _ = fs::remove_file(&self.record);
        // An unrelated fork may have inherited the same open file description.
        // Closing our descriptor alone then leaves the lock active past this
        // owner's withdrawal. Explicitly unlock only after removing the route.
        unsafe { libc::flock(self._lock.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ShortcutChoice;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn alternate_route_is_explicit_and_default_records_are_unchanged() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let socket = root.path().join("frontend.sock");
        let pid = std::process::id();
        let local = InputLease::acquire(root.path(), &socket, ShortcutChoice::Default).unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("voicetype-app-input/owner")).unwrap(),
            format!("voicetype-input-v1\n{pid}\n{}\n", socket.display())
        );
        drop(local);
        let google =
            InputLease::acquire_google(root.path(), &socket, ShortcutChoice::CtrlShiftF9).unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("voicetype-app-input/owner")).unwrap(),
            format!(
                "voicetype-input-v3\n{pid}\n{}\ngoogle-toggle-ctrl-shift-f9\n",
                socket.display()
            )
        );
        google.rebind(ShortcutChoice::Default).unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("voicetype-app-input/owner")).unwrap(),
            format!(
                "voicetype-input-v2\n{pid}\n{}\ngoogle-toggle\n",
                socket.display()
            )
        );
        drop(google);
        assert!(!root.path().join("voicetype-app-input/owner").exists());
    }
}
