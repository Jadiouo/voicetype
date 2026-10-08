//! Verified, versioned asset installation. Download/extraction prepare a source
//! directory; this boundary copies only pinned files and atomically activates a
//! complete version. It never modifies a running version or personal data.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssetManifest {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub platform: String,
    pub source_url: String,
    pub license: String,
    pub files: Vec<AssetFile>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssetFile {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    pub executable: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Activation {
    schema_version: u32,
    current: String,
    previous: Option<String>,
}

pub struct AssetStore {
    root: PathBuf,
}

impl AssetStore {
    pub fn open(root: PathBuf) -> io::Result<Self> {
        if !root.is_absolute() {
            return Err(invalid("asset directory must be absolute"));
        }
        private_dir(&root)?;
        private_dir(&root.join("versions"))?;
        Ok(Self { root })
    }

    /// Manifest comes from the reviewed application catalog, never downloaded
    /// alongside arbitrary files and trusted merely because its hashes match.
    pub fn install(&self, manifest: &AssetManifest, source: &Path) -> io::Result<PathBuf> {
        self.install_with_progress(manifest, source, |_, _| true)
    }

    /// Returning false cancels before publication, including during copying or
    /// verification of an existing version. Success is the commit point; a late
    /// cancellation must not be reported as if an activated version was undone.
    pub fn install_with_progress(
        &self,
        manifest: &AssetManifest,
        source: &Path,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> io::Result<PathBuf> {
        manifest.validate()?;
        let total = manifest.files.iter().map(|file| file.bytes).sum();
        report(&mut progress, 0, total)?;
        let _lock = self.lock()?;
        let old_bytes = optional_read(&self.root.join("active.json"))?;
        let previous = decode_activation(old_bytes.as_deref())?.map(|value| value.current);
        let encoded = serde_json::to_vec(manifest).map_err(io::Error::other)?;
        let digest = format!("{:x}", Sha256::digest(&encoded));
        if let Some(current) = &previous {
            if version_key(current)? == digest {
                match self.verify_version(current, &mut progress) {
                    Ok(installed) => return Ok(installed),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
                    Err(_) => {}
                }
            }
        }
        let staged = tempfile::Builder::new()
            .prefix(&format!("{digest}-"))
            .tempdir_in(self.root.join("versions"))?;
        let mut done = 0;
        for file in &manifest.files {
            let mut input = open_regular(source, &file.path)?;
            if input.metadata()?.len() != file.bytes {
                return Err(invalid("asset size mismatch"));
            }
            let destination = staged.path().join(&file.path);
            private_dir(destination.parent().unwrap())?;
            let mut output = File::create(&destination)?;
            verify_stream(&mut input, file, Some(&mut output), &mut |bytes| {
                report(&mut progress, done + bytes, total)
            })?;
            done += file.bytes;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                output.set_permissions(fs::Permissions::from_mode(if file.executable {
                    0o700
                } else {
                    0o600
                }))?;
            }
            output.sync_all()?;
            let mut directory = destination.parent().unwrap();
            loop {
                sync_dir(directory)?;
                if directory == staged.path() {
                    break;
                }
                directory = directory.parent().unwrap();
            }
        }
        let mut receipt = File::create(staged.path().join("manifest.json"))?;
        receipt.write_all(&encoded)?;
        receipt.sync_all()?;
        drop(receipt);
        sync_dir(staged.path())?;
        report(&mut progress, total, total)?;
        // Keep before publishing: a destructor must never erase the active
        // directory after a successful atomic pointer replacement.
        let installed = staged.keep();
        sync_dir(&self.root.join("versions"))?;
        let current = installed.file_name().unwrap().to_str().unwrap().to_owned();
        self.activate(
            Activation {
                schema_version: 1,
                current,
                previous,
            },
            old_bytes.as_deref(),
        )?;
        Ok(installed)
    }

    /// Full integrity check for setup/activation, not a per-dictation/status poll.
    pub fn active(&self) -> io::Result<Option<PathBuf>> {
        self.active_with_progress(|_, _| true)
    }

    pub fn active_with_progress(
        &self,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> io::Result<Option<PathBuf>> {
        let bytes = optional_read(&self.root.join("active.json"))?;
        decode_activation(bytes.as_deref())?
            .map(|value| self.verify_version(&value.current, &mut progress))
            .transpose()
    }

    /// Unlike active(), this also checks that the receipt is the exact catalog
    /// version the application requested, before trusting any installed paths.
    pub fn matching(
        &self,
        manifest: &AssetManifest,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> io::Result<Option<PathBuf>> {
        manifest.validate()?;
        let bytes = optional_read(&self.root.join("active.json"))?;
        let Some(active) = decode_activation(bytes.as_deref())? else {
            return Ok(None);
        };
        let encoded = serde_json::to_vec(manifest).map_err(io::Error::other)?;
        if version_key(&active.current)? != format!("{:x}", Sha256::digest(encoded)) {
            return Ok(None);
        }
        self.verify_version(&active.current, &mut progress)
            .map(Some)
    }

    pub fn rollback(&self) -> io::Result<PathBuf> {
        let _lock = self.lock()?;
        let bytes = optional_read(&self.root.join("active.json"))?;
        let active = decode_activation(bytes.as_deref())?.ok_or(io::ErrorKind::NotFound)?;
        let previous = active.previous.ok_or(io::ErrorKind::NotFound)?;
        let path = self.verify_version(&previous, &mut |_, _| true)?;
        self.activate(
            Activation {
                schema_version: 1,
                current: previous,
                previous: Some(active.current),
            },
            bytes.as_deref(),
        )?;
        Ok(path)
    }

    fn verify_version(
        &self,
        key: &str,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> io::Result<PathBuf> {
        let digest = version_key(key)?;
        let root = self.root.join("versions").join(key);
        let mut receipt = open_regular(&root, "manifest.json")?;
        if receipt.metadata()?.len() > 1024 * 1024 {
            return Err(invalid("asset manifest too large"));
        }
        let mut encoded = Vec::new();
        (&mut receipt)
            .take(1024 * 1024 + 1)
            .read_to_end(&mut encoded)?;
        if encoded.len() > 1024 * 1024 {
            return Err(invalid("asset manifest too large"));
        }
        if format!("{:x}", Sha256::digest(&encoded)) != digest {
            return Err(invalid("installed manifest changed"));
        }
        let manifest: AssetManifest = serde_json::from_slice(&encoded).map_err(io::Error::other)?;
        manifest.validate()?;
        let total = manifest.files.iter().map(|file| file.bytes).sum();
        let mut done = 0;
        report(progress, 0, total)?;
        for file in &manifest.files {
            let mut input = open_regular(&root, &file.path)?;
            if input.metadata()?.len() != file.bytes {
                return Err(invalid("installed asset size changed"));
            }
            verify_stream(&mut input, file, None, &mut |bytes| {
                report(progress, done + bytes, total)
            })?;
            done += file.bytes;
        }
        Ok(root)
    }

    fn lock(&self) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(self.root.join("install.lock"))?;
        file.try_lock().map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "asset installation already in progress",
            )
        })?;
        Ok(file)
    }

    fn activate(&self, value: Activation, previous: Option<&[u8]>) -> io::Result<()> {
        let path = self.root.join("active.json");
        if optional_read(&path)?.as_deref() != previous {
            return Err(invalid("asset activation changed concurrently"));
        }
        let mut file = tempfile::NamedTempFile::new_in(&self.root)?;
        serde_json::to_writer(file.as_file_mut(), &value).map_err(io::Error::other)?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|error| error.error)?;
        sync_dir(&self.root)
    }
}

impl AssetManifest {
    pub(crate) fn validate(&self) -> io::Result<()> {
        if self.schema_version != 1
            || self.id.is_empty()
            || self.id.len() > 160
            || self.version.is_empty()
            || self.version.len() > 160
            || self.license.is_empty()
            || !self.source_url.starts_with("https://")
            || self.files.is_empty()
            || self.files.len() > 256
        {
            return Err(invalid("unsupported asset manifest"));
        }
        let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        if self.platform != "any" && self.platform != platform {
            return Err(invalid("asset platform mismatch"));
        }
        let mut paths = HashSet::new();
        let mut total = 0u64;
        for file in &self.files {
            portable_path(&file.path)?;
            if file
                .path
                .split('/')
                .next()
                .unwrap()
                .eq_ignore_ascii_case("manifest.json")
                || !paths.insert(file.path.to_ascii_lowercase())
                || !sha256(&file.sha256)
                || file.bytes == 0
                || file.bytes > 16 * 1024 * 1024 * 1024
            {
                return Err(invalid("invalid asset entry"));
            }
            total = total
                .checked_add(file.bytes)
                .ok_or_else(|| invalid("asset bundle too large"))?;
        }
        if total > 32 * 1024 * 1024 * 1024 {
            return Err(invalid("asset bundle too large"));
        }
        Ok(())
    }
}

pub(crate) fn portable_path(value: &str) -> io::Result<()> {
    if value.is_empty()
        || value.len() > 240
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"/_-.+".contains(&c))
    {
        return Err(invalid("invalid asset path"));
    }
    for part in value.split('/') {
        let stem = part.split('.').next().unwrap().to_ascii_uppercase();
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with('.')
            || ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit())
        {
            return Err(invalid("non-portable asset path"));
        }
    }
    Ok(())
}

fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn version_key(key: &str) -> io::Result<&str> {
    let (digest, suffix) = key
        .split_once('-')
        .ok_or_else(|| invalid("invalid installed version"))?;
    if !sha256(digest)
        || suffix.is_empty()
        || suffix.len() > 32
        || !suffix.bytes().all(|c| c.is_ascii_alphanumeric())
    {
        return Err(invalid("invalid installed version"));
    }
    Ok(digest)
}

fn open_regular(root: &Path, relative: &str) -> io::Result<File> {
    // No links in the staged/installed path. Only pinned regular files are read.
    let mut path = root.to_owned();
    if !fs::symlink_metadata(&path)?.is_dir() {
        return Err(invalid("asset root is not a directory"));
    }
    for part in relative.split('/') {
        path.push(part);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(invalid("asset links are not allowed"));
        }
    }
    if !fs::symlink_metadata(&path)?.is_file() {
        return Err(invalid("asset is not a regular file"));
    }
    File::open(path)
}

fn verify_stream(
    input: &mut File,
    expected: &AssetFile,
    mut output: Option<&mut File>,
    progress: &mut dyn FnMut(u64) -> io::Result<()>,
) -> io::Result<()> {
    let mut hash = Sha256::new();
    let mut read = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        progress(read)?;
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        read += count as u64;
        if read > expected.bytes {
            return Err(invalid("asset exceeded expected size"));
        }
        hash.update(&buffer[..count]);
        if let Some(file) = output.as_mut() {
            file.write_all(&buffer[..count])?;
        }
    }
    if read != expected.bytes || format!("{:x}", hash.finalize()) != expected.sha256 {
        return Err(invalid("asset integrity mismatch"));
    }
    Ok(())
}

fn report(progress: &mut dyn FnMut(u64, u64) -> bool, done: u64, total: u64) -> io::Result<()> {
    if progress(done, total) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "asset installation cancelled",
        ))
    }
}

fn private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(invalid("asset directory is not a directory"));
    }
    Ok(())
}

fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn optional_read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match File::open(path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take(4097).read_to_end(&mut bytes)?;
            if bytes.len() > 4096 {
                return Err(invalid("activation record too large"));
            }
            Ok(Some(bytes))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn decode_activation(bytes: Option<&[u8]>) -> io::Result<Option<Activation>> {
    bytes
        .map(|bytes| {
            let value: Activation = serde_json::from_slice(bytes).map_err(io::Error::other)?;
            if value.schema_version != 1 {
                return Err(invalid("unsupported activation record"));
            }
            version_key(&value.current)?;
            if let Some(previous) = &value.previous {
                version_key(previous)?;
            }
            Ok(value)
        })
        .transpose()
}
