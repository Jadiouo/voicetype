//! Explicit, journaled ownership transfer from the old Google shortcut/service.
//! Callers supply verified Fcitx identity and an exact service controller. A
//! preference change never constructs or invokes this object.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

const MAX_CONFIG: usize = 16 * 1024;
// JSON encodes both 16 KiB byte arrays as decimal numbers, not base64.
const MAX_JOURNAL: usize = 256 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FcitxIdentity {
    pub pid: u32,
    pub start_ticks: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UnitIdentity {
    pub fragment: PathBuf,
    pub unit_definition_sha256: String,
    pub unit_file_state: String,
    pub enabled: bool,
    pub active: bool,
    pub cgroup: Option<PathBuf>,
}

pub trait LegacyService {
    fn snapshot(&self) -> io::Result<UnitIdentity>;
    fn fcitx_identity(&self) -> io::Result<FcitxIdentity>;
    fn stop(&self) -> io::Result<()>;
    /// Both the exact unit cgroup and legacy listener must be quiet.
    fn quiescent(&self, original_cgroup: Option<&Path>) -> io::Result<bool>;
    fn start(&self) -> io::Result<()>;
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferPhase {
    DisableStaged,
    AppOwned,
    RestoreStaged,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema_version: u32,
    config_path: PathBuf,
    original: Option<Vec<u8>>,
    original_sha256: Option<String>,
    managed: Vec<u8>,
    managed_sha256: String,
    original_mode: Option<u32>,
    fcitx_before: FcitxIdentity,
    fcitx_owned: Option<FcitxIdentity>,
    #[serde(default)]
    fcitx_restore_after: Option<FcitxIdentity>,
    unit_before: UnitIdentity,
    stop_attempted: bool,
    service_claimed: bool,
    phase: TransferPhase,
}

pub struct GoogleShortcutTransfer {
    profile: PathBuf,
    config: PathBuf,
    bridge_lock: PathBuf,
}

/// Owns the *same* bridge.lock inode for the whole App Google lifetime.
/// Dropping this without an explicit restore keeps the journal, not a claim
/// that the old service has been restarted.
pub struct LegacyOwnership {
    bridge_lock: Option<File>,
}

impl GoogleShortcutTransfer {
    pub fn new(profile: PathBuf, config: PathBuf, bridge_lock: PathBuf) -> io::Result<Self> {
        if !profile.is_absolute() || !config.is_absolute() || !bridge_lock.is_absolute() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Self {
            profile,
            config,
            bridge_lock,
        })
    }

    pub fn phase(&self) -> io::Result<Option<TransferPhase>> {
        let _lock = self.settings_lock()?;
        self.journal()?
            .map(|journal| {
                self.validate(&journal)?;
                Ok(journal.phase)
            })
            .transpose()
    }

    /// Journal exact original bytes before changing the single top-level
    /// Enabled value. Retry after a crash never replaces the first backup.
    pub fn stage_disable(&self, fcitx: FcitxIdentity, unit: UnitIdentity) -> io::Result<()> {
        if fcitx.pid == 0
            || fcitx.start_ticks == 0
            || unit.unit_definition_sha256.len() != 64
            || !matches!(
                unit.unit_file_state.as_str(),
                "enabled" | "enabled-runtime" | "disabled"
            )
            || unit.enabled != unit.unit_file_state.starts_with("enabled")
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let _lock = self.settings_lock()?;
        let current = read_config(&self.config)?;
        let journal = match self.journal()? {
            Some(journal) => {
                self.validate(&journal)?;
                if journal.phase != TransferPhase::DisableStaged
                    || journal.fcitx_before != fcitx
                    || journal.unit_before != unit
                    || (current.as_ref() != journal.original.as_ref()
                        && current.as_deref() != Some(journal.managed.as_slice()))
                {
                    return Err(io::ErrorKind::AlreadyExists.into());
                }
                journal
            }
            None => {
                let managed = disabled_bytes(current.as_deref())?;
                if managed.len() > MAX_CONFIG {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                let journal = Journal {
                    schema_version: 1,
                    config_path: self.config.clone(),
                    original_sha256: current.as_ref().map(|bytes| digest(bytes)),
                    original_mode: config_mode(&self.config)?,
                    original: current.clone(),
                    managed_sha256: digest(&managed),
                    managed,
                    fcitx_before: fcitx,
                    fcitx_owned: None,
                    fcitx_restore_after: None,
                    unit_before: unit,
                    stop_attempted: false,
                    service_claimed: false,
                    phase: TransferPhase::DisableStaged,
                };
                self.write_journal(&journal)?;
                journal
            }
        };
        if current.as_deref() == Some(journal.managed.as_slice()) {
            return Ok(());
        }
        if current != journal.original {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        atomic_write(
            &self.config,
            &journal.managed,
            journal.original_mode.unwrap_or(0o600),
        )
    }

    /// Fcitx must have reloaded the disabled bytes and completed a v2 route
    /// handshake. Service stop is explicit and cancellation of an old sentence
    /// is disclosed by the caller before this method is invoked.
    pub fn claim<S: LegacyService>(
        &self,
        current_fcitx: FcitxIdentity,
        v2_handshake: bool,
        service: &S,
    ) -> io::Result<LegacyOwnership> {
        let _lock = self.settings_lock()?;
        let mut journal = self.journal()?.ok_or(io::ErrorKind::NotFound)?;
        self.validate(&journal)?;
        if journal.phase != TransferPhase::DisableStaged
            || !v2_handshake
            || current_fcitx.pid == 0
            || current_fcitx.start_ticks == 0
            || current_fcitx == journal.fcitx_before
            || read_config(&self.config)?.as_deref() != Some(journal.managed.as_slice())
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let before = service.snapshot()?;
        if !same_unit(&before, &journal.unit_before)
            || (before.active && !journal.unit_before.active)
        {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        if !journal.stop_attempted {
            journal.stop_attempted = true;
            self.write_journal(&journal)?;
        }
        service.stop()?;
        if !service.quiescent(journal.unit_before.cgroup.as_deref())? {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let bridge = open_bridge_lock(&self.bridge_lock)?;
        bridge.try_lock().map_err(|_| io::ErrorKind::WouldBlock)?;
        let after = fs::symlink_metadata(&self.bridge_lock)?;
        if bridge.metadata()?.ino() != after.ino() || bridge.metadata()?.dev() != after.dev() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        // A stop/lock race must not turn a restarted service into App ownership.
        if !service.quiescent(journal.unit_before.cgroup.as_deref())?
            || !same_unit(&service.snapshot()?, &journal.unit_before)
        {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        journal.phase = TransferPhase::AppOwned;
        journal.service_claimed = true;
        journal.fcitx_owned = Some(current_fcitx);
        self.write_journal(&journal)?;
        Ok(LegacyOwnership {
            bridge_lock: Some(bridge),
        })
    }

    /// After an App crash, an explicit restore may recover the lifetime lock.
    /// A still-running App or restarted legacy service makes this fail closed.
    pub fn reclaim_for_restore<S: LegacyService>(
        &self,
        service: &S,
    ) -> io::Result<LegacyOwnership> {
        let _lock = self.settings_lock()?;
        let journal = self.journal()?.ok_or(io::ErrorKind::NotFound)?;
        self.validate(&journal)?;
        if journal.phase != TransferPhase::AppOwned
            || read_config(&self.config)?.as_deref() != Some(journal.managed.as_slice())
            || !same_unit(&service.snapshot()?, &journal.unit_before)
            || !service.quiescent(journal.unit_before.cgroup.as_deref())?
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let bridge = open_bridge_lock(&self.bridge_lock)?;
        bridge.try_lock().map_err(|_| io::ErrorKind::WouldBlock)?;
        let after = fs::symlink_metadata(&self.bridge_lock)?;
        if bridge.metadata()?.ino() != after.ino() || bridge.metadata()?.dev() != after.dev() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        if !service.quiescent(journal.unit_before.cgroup.as_deref())?
            || !same_unit(&service.snapshot()?, &journal.unit_before)
        {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        Ok(LegacyOwnership {
            bridge_lock: Some(bridge),
        })
    }

    /// Explicitly cancel a staged handoff before any service ownership was
    /// claimed. The journal remains until Fcitx reloads the restored bytes.
    pub fn abort_staged<S: LegacyService>(
        &self,
        current_fcitx: FcitxIdentity,
        app_quiescent: bool,
        service: &S,
    ) -> io::Result<()> {
        let _lock = self.settings_lock()?;
        let mut journal = self.journal()?.ok_or(io::ErrorKind::NotFound)?;
        self.validate(&journal)?;
        let current = read_config(&self.config)?;
        let unit = service.snapshot()?;
        if !app_quiescent
            || journal.phase != TransferPhase::DisableStaged
            || current_fcitx.pid == 0
            || current_fcitx.start_ticks == 0
            || (current != journal.original
                && current.as_deref() != Some(journal.managed.as_slice()))
            || !same_unit(&unit, &journal.unit_before)
            || (unit.active && !journal.unit_before.active)
            || (journal.unit_before.active && !unit.active && !journal.stop_attempted)
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let needs_restart = journal.unit_before.active && !unit.active;
        let bridge = if needs_restart {
            if !service.quiescent(journal.unit_before.cgroup.as_deref())? {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let bridge = open_bridge_lock(&self.bridge_lock)?;
            bridge.try_lock().map_err(|_| io::ErrorKind::WouldBlock)?;
            if !service.quiescent(journal.unit_before.cgroup.as_deref())? {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            Some(bridge)
        } else {
            None
        };
        journal.phase = TransferPhase::RestoreStaged;
        journal.fcitx_owned = Some(current_fcitx);
        self.write_journal(&journal)?;
        if current.as_deref() == Some(journal.managed.as_slice()) {
            restore_config(&self.config, &journal)?;
        }
        journal.fcitx_restore_after = service.fcitx_identity().ok();
        self.write_journal(&journal)?;
        drop(bridge);
        if needs_restart {
            service.start()?;
        }
        Ok(())
    }

    /// Caller must first prove App route, recorder and CLI are fully reaped.
    /// A restore leaves its journal until a later Fcitx identity confirms reload.
    pub fn stage_restore<S: LegacyService>(
        &self,
        ownership: &mut LegacyOwnership,
        app_quiescent: bool,
        service: &S,
    ) -> io::Result<()> {
        let _lock = self.settings_lock()?;
        let mut journal = self.journal()?.ok_or(io::ErrorKind::NotFound)?;
        self.validate(&journal)?;
        let current = read_config(&self.config)?;
        if !app_quiescent
            || !matches!(
                journal.phase,
                TransferPhase::AppOwned | TransferPhase::RestoreStaged
            )
            || (current.as_deref() != Some(journal.managed.as_slice())
                && current != journal.original)
            || !same_unit(&service.snapshot()?, &journal.unit_before)
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if journal.phase == TransferPhase::AppOwned {
            if ownership.bridge_lock.is_none()
                || current.as_deref() != Some(journal.managed.as_slice())
                || !service.quiescent(journal.unit_before.cgroup.as_deref())?
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            journal.phase = TransferPhase::RestoreStaged;
            self.write_journal(&journal)?;
        }
        if current.as_deref() == Some(journal.managed.as_slice()) {
            if ownership.bridge_lock.is_none()
                || !service.quiescent(journal.unit_before.cgroup.as_deref())?
            {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            restore_config(&self.config, &journal)?;
        }
        if journal.fcitx_restore_after.is_none() {
            journal.fcitx_restore_after = service.fcitx_identity().ok();
            self.write_journal(&journal)?;
        }
        ownership.bridge_lock.take();
        if journal.unit_before.active && !service.snapshot()?.active {
            service.start()?;
        }
        Ok(())
    }

    /// Resume only an explicit restore after an interrupted write/start. The
    /// old unit is never restarted merely because the App process reopened.
    pub fn resume_restore<S: LegacyService>(
        &self,
        app_quiescent: bool,
        service: &S,
    ) -> io::Result<()> {
        let _lock = self.settings_lock()?;
        let mut journal = self.journal()?.ok_or(io::ErrorKind::NotFound)?;
        self.validate(&journal)?;
        if !app_quiescent
            || journal.phase != TransferPhase::RestoreStaged
            || !same_unit(&service.snapshot()?, &journal.unit_before)
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let current = read_config(&self.config)?;
        if !journal.service_claimed {
            let unit = service.snapshot()?;
            let needs_restart = journal.unit_before.active && !unit.active;
            if (unit.active && !journal.unit_before.active)
                || (needs_restart && !journal.stop_attempted)
            {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let bridge = if needs_restart {
                if !service.quiescent(journal.unit_before.cgroup.as_deref())? {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                let bridge = open_bridge_lock(&self.bridge_lock)?;
                bridge.try_lock().map_err(|_| io::ErrorKind::WouldBlock)?;
                if !service.quiescent(journal.unit_before.cgroup.as_deref())? {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                Some(bridge)
            } else {
                None
            };
            if current.as_deref() == Some(journal.managed.as_slice()) {
                restore_config(&self.config, &journal)?;
            } else if current != journal.original {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            if journal.fcitx_restore_after.is_none() {
                journal.fcitx_restore_after = service.fcitx_identity().ok();
                self.write_journal(&journal)?;
            }
            drop(bridge);
            if needs_restart {
                service.start()?;
            }
            return Ok(());
        }
        if current.as_deref() == Some(journal.managed.as_slice()) {
            if !service.quiescent(journal.unit_before.cgroup.as_deref())? {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let bridge = open_bridge_lock(&self.bridge_lock)?;
            bridge.try_lock().map_err(|_| io::ErrorKind::WouldBlock)?;
            if !service.quiescent(journal.unit_before.cgroup.as_deref())? {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            restore_config(&self.config, &journal)?;
            drop(bridge);
        } else if current != journal.original {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        if journal.fcitx_restore_after.is_none() {
            journal.fcitx_restore_after = service.fcitx_identity().ok();
            self.write_journal(&journal)?;
        }
        if journal.unit_before.active && !service.snapshot()?.active {
            service.start()?;
        }
        Ok(())
    }

    pub fn finish_restore<S: LegacyService>(
        &self,
        current_fcitx: FcitxIdentity,
        service: &S,
    ) -> io::Result<()> {
        let _lock = self.settings_lock()?;
        let journal = self.journal()?.ok_or(io::ErrorKind::NotFound)?;
        self.validate(&journal)?;
        if journal.phase != TransferPhase::RestoreStaged
            || current_fcitx == journal.fcitx_before
            || current_fcitx.pid == 0
            || current_fcitx.start_ticks == 0
            || journal.fcitx_owned.as_ref() == Some(&current_fcitx)
            || journal
                .fcitx_restore_after
                .as_ref()
                .is_none_or(|seen| seen == &current_fcitx)
            || read_config(&self.config)? != journal.original
            || !same_unit(&service.snapshot()?, &journal.unit_before)
            || service.snapshot()?.active != journal.unit_before.active
        {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        fs::remove_file(self.journal_path())?;
        File::open(&self.profile)?.sync_all()
    }

    fn validate(&self, journal: &Journal) -> io::Result<()> {
        if journal.schema_version != 1
            || journal.config_path != self.config
            || journal
                .original
                .as_ref()
                .is_some_and(|v| v.len() > MAX_CONFIG)
            || journal.managed.len() > MAX_CONFIG
            || journal.managed_sha256 != digest(&journal.managed)
            || journal.original_sha256 != journal.original.as_ref().map(|v| digest(v))
            || (journal.phase != TransferPhase::DisableStaged && journal.fcitx_owned.is_none())
            || (journal.phase == TransferPhase::AppOwned && !journal.service_claimed)
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(())
    }

    fn journal_path(&self) -> PathBuf {
        self.profile.join("google-shortcut-rollback.json")
    }
    fn write_journal(&self, journal: &Journal) -> io::Result<()> {
        let bytes = serde_json::to_vec(journal)?;
        if bytes.len() > MAX_JOURNAL {
            return Err(io::ErrorKind::InvalidData.into());
        }
        atomic_write(&self.journal_path(), &bytes, 0o600)
    }
    fn journal(&self) -> io::Result<Option<Journal>> {
        if let Ok(meta) = fs::symlink_metadata(self.journal_path()) {
            if !meta.is_file()
                || meta.uid() != unsafe { libc::geteuid() }
                || meta.permissions().mode() & 0o077 != 0
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        }
        read_limited(&self.journal_path(), MAX_JOURNAL)?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(io::Error::other))
            .transpose()
    }
    fn settings_lock(&self) -> io::Result<File> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.profile)?;
        let profile = fs::symlink_metadata(&self.profile)?;
        if !profile.is_dir()
            || profile.uid() != unsafe { libc::geteuid() }
            || profile.permissions().mode() & 0o077 != 0
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.profile.join("fcitx-install.lock"))?;
        if !lock.metadata()?.is_file() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        lock.try_lock().map_err(|_| io::ErrorKind::WouldBlock)?;
        Ok(lock)
    }
}

fn restore_config(path: &Path, journal: &Journal) -> io::Result<()> {
    match &journal.original {
        Some(bytes) => atomic_write(path, bytes, journal.original_mode.unwrap_or(0o600)),
        None => {
            fs::remove_file(path)?;
            File::open(path.parent().ok_or(io::ErrorKind::InvalidInput)?)?.sync_all()
        }
    }
}

fn same_unit(current: &UnitIdentity, original: &UnitIdentity) -> bool {
    current.fragment == original.fragment
        && current.unit_definition_sha256 == original.unit_definition_sha256
        && current.unit_file_state == original.unit_file_state
}

fn disabled_bytes(original: Option<&[u8]>) -> io::Result<Vec<u8>> {
    let Some(bytes) = original else {
        return Ok(b"Enabled=False\n".to_vec());
    };
    if bytes.len() > MAX_CONFIG {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut result = bytes.to_vec();
    let mut section = false;
    let mut found = None;
    let mut offset = 0;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let content = line.strip_suffix(b"\n").unwrap_or(line);
        let content = content.strip_suffix(b"\r").unwrap_or(content);
        let trimmed = trim_ascii(content);
        if trimmed.starts_with(b"[") && trimmed.ends_with(b"]") {
            section = true;
        }
        if !section {
            if let Some(eq) = content.iter().position(|byte| *byte == b'=') {
                if trim_ascii(&content[..eq]) == b"Enabled" {
                    if found.is_some() {
                        return Err(io::ErrorKind::InvalidData.into());
                    }
                    let value = trim_ascii(&content[eq + 1..]);
                    if value != b"True" && value != b"False" {
                        return Err(io::ErrorKind::InvalidData.into());
                    }
                    let start = offset
                        + eq
                        + 1
                        + content[eq + 1..]
                            .iter()
                            .take_while(|b| b.is_ascii_whitespace())
                            .count();
                    found = Some((start, value.len()));
                }
            }
        }
        offset += line.len();
    }
    let (start, len) = found.ok_or(io::ErrorKind::InvalidData)?;
    result.splice(start..start + len, b"False".iter().copied());
    Ok(result)
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn config_mode(path: &Path) -> io::Result<Option<u32>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && meta.uid() == unsafe { libc::geteuid() } => {
            Ok(Some(meta.permissions().mode() & 0o777))
        }
        Ok(_) => Err(io::ErrorKind::PermissionDenied.into()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
fn read_config(path: &Path) -> io::Result<Option<Vec<u8>>> {
    read_limited(path, MAX_CONFIG)
}
fn read_limited(path: &Path, limit: usize) -> io::Result<Option<Vec<u8>>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(Some(bytes))
}
fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(parent)?;
    let mut stage = tempfile::NamedTempFile::new_in(parent)?;
    stage
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    stage.write_all(bytes)?;
    stage.as_file().sync_all()?;
    stage.persist(path).map_err(|e| e.error)?;
    File::open(parent)?.sync_all()
}
fn open_bridge_lock(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(false)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.nlink() != 1
        || meta.mode() & 0o777 != 0o600
        || meta.ino() != current.ino()
        || meta.dev() != current.dev()
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(file)
}
