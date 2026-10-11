//! Explicit per-user Fcitx registration. Immutable verified modules, a write-ahead
//! rollback record and drift detection; never restart Fcitx or alter its settings.
use crate::assets::{AssetManifest, AssetStore};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub struct FcitxInstaller {
    profile: PathBuf,
    registration: PathBuf,
    manifest: AssetManifest,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rollback {
    schema_version: u32,
    destination: PathBuf,
    original: Option<Vec<u8>>,
    // Includes a newly journaled version whose publication may have been
    // interrupted. Updating the module never replaces the original backup.
    managed: Vec<Vec<u8>>,
}

impl FcitxInstaller {
    pub fn new(
        profile: PathBuf,
        registration: PathBuf,
        manifest: AssetManifest,
    ) -> io::Result<Self> {
        manifest.validate()?;
        if !profile.is_absolute()
            || !registration.is_absolute()
            || manifest.id != "fcitx-input"
            || !manifest.files.iter().any(|f| f.path == "libvoicetype.so")
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Self {
            profile,
            registration,
            manifest,
        })
    }

    pub fn install(&self, source: &Path) -> io::Result<PathBuf> {
        let _lock = self.lock()?;
        let original = optional_read(&self.registration)?;
        if original
            .as_ref()
            .is_some_and(|bytes| bytes.len() > 64 * 1024)
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut rollback = self.rollback()?.unwrap_or(Rollback {
            schema_version: 1,
            destination: self.registration.clone(),
            original: original.clone(),
            managed: Vec::new(),
        });
        self.check_current(&rollback, &original)?;
        let installed =
            AssetStore::open(self.profile.join("input-assets"))?.install(&self.manifest, source)?;
        let library = installed.join("libvoicetype");
        let library = library
            .to_str()
            .filter(|s| !s.contains(['\n', '\r', '\0']))
            .ok_or(io::ErrorKind::InvalidInput)?;
        let registration = format!("[Addon]\nName=VoiceType\nName[zh_TW]=語音聽寫\nCategory=Module\nType=SharedLibrary\nLibrary={library}\nOnDemand=False\nConfigurable=True\n\n[Addon/OptionalDependencies]\n0=notifications\n1=xcb\n").into_bytes();
        if !rollback.managed.contains(&registration) {
            if rollback.managed.len() >= 128 {
                return Err(io::ErrorKind::InvalidData.into());
            }
            rollback.managed.push(registration.clone());
        }
        // Crash at either boundary is retryable and preserves the original.
        let journal = serde_json::to_vec(&rollback)?;
        if journal.len() > 1024 * 1024 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        atomic_write(&self.profile.join("fcitx-rollback.json"), &journal)?;
        fs::create_dir_all(
            self.registration
                .parent()
                .ok_or(io::ErrorKind::InvalidInput)?,
        )?;
        // Detect an external change made during asset verification/journaling.
        if optional_read(&self.registration)? != original {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        atomic_write(&self.registration, &registration)?;
        Ok(installed)
    }

    pub fn restore(&self) -> io::Result<bool> {
        let _lock = self.lock()?;
        let Some(rollback) = self.rollback()? else {
            return Ok(false);
        };
        self.check_current(&rollback, &optional_read(&self.registration)?)?;
        match rollback.original {
            Some(bytes) => atomic_write(&self.registration, &bytes)?,
            None => match fs::remove_file(&self.registration) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            },
        }
        fs::remove_file(self.profile.join("fcitx-rollback.json"))?;
        Ok(true)
    }

    fn rollback(&self) -> io::Result<Option<Rollback>> {
        optional_read(&self.profile.join("fcitx-rollback.json"))?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(io::Error::other))
            .transpose()
    }
    fn check_current(&self, record: &Rollback, current: &Option<Vec<u8>>) -> io::Result<()> {
        if record.schema_version != 1
            || record.destination != self.registration
            || !(current == &record.original
                || current.as_ref().is_some_and(|v| record.managed.contains(v)))
        {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        Ok(())
    }
    fn lock(&self) -> io::Result<File> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.profile)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.profile.join("fcitx-install.lock"))?;
        if !file.metadata()?.is_file() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        file.try_lock()
            .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
        Ok(file)
    }
}

fn optional_read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !file.metadata()?.is_file() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(Some(bytes))
}
fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged.persist(path).map_err(|e| e.error)?;
    File::open(parent)?.sync_all()
}
