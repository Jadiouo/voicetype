//! Temporary Fcitx routing. Legacy configuration and services are never changed.
//! The record is published before handoff and names this live owner. Removing it
//! (or losing its socket) rolls back to Fcitx's unchanged legacy endpoint.
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
    _lock: File,
}

impl InputLease {
    pub fn acquire(runtime: &Path, socket: &Path) -> io::Result<Self> {
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
            .ok_or(io::ErrorKind::InvalidInput)?;
        let record = directory.join("owner");
        let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
        writeln!(
            staged,
            "voicetype-input-v1\n{}\n{socket}",
            std::process::id()
        )?;
        staged.as_file().sync_all()?;
        staged.persist(&record).map_err(|e| e.error)?;
        Ok(Self {
            record,
            _lock: lock,
        })
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
    }
}
