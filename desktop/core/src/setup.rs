//! Bounded model preparation, separate from recording and the runtime owner.
use crate::assets::{portable_path, AssetManifest, AssetStore};
pub use crate::setup_worker::{bundled_models, ModelBundle, ModelSetup, ModelSource, SetupStatus};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadSpec {
    pub format: DownloadFormat,
    #[serde(default)]
    pub strip_prefix: Option<String>,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Copy, Deserialize)]
pub enum DownloadFormat {
    #[serde(rename = "file")]
    File,
    #[serde(rename = "tar.bz2")]
    TarBz2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupPhase {
    NotChecked,
    CheckingInstalled,
    Downloading,
    CheckingDownload,
    Extracting,
    Installing,
    Installed,
    Cancelled,
    Failed,
}

impl DownloadSpec {
    pub(crate) fn validate(&self, manifest: &AssetManifest) -> io::Result<()> {
        manifest.validate()?;
        if self.bytes == 0
            || self.bytes > 16 * 1024 * 1024 * 1024
            || self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(invalid("invalid download manifest"));
        }
        match self.format {
            DownloadFormat::File if self.strip_prefix.is_none() && manifest.files.len() == 1 => {
                Ok(())
            }
            DownloadFormat::TarBz2 => {
                let prefix = self
                    .strip_prefix
                    .as_deref()
                    .ok_or_else(|| invalid("missing archive prefix"))?;
                portable_path(prefix)?;
                if prefix.contains('/') {
                    return Err(invalid("archive prefix must be one directory"));
                }
                Ok(())
            }
            _ => Err(invalid("unsupported download layout")),
        }
    }
}

/// Native callers supply the reviewed catalog, never webview paths/manifests.
/// A cached/downloaded file is still untrusted: verify the entire compressed
/// body before interpreting it, then verify every extracted file before commit.
pub fn install_download(
    store: &AssetStore,
    manifest: &AssetManifest,
    spec: &DownloadSpec,
    download: &Path,
    mut progress: impl FnMut(SetupPhase, u64, u64) -> bool,
) -> io::Result<PathBuf> {
    spec.validate(manifest)?;
    let mut input = File::open(download)?;
    if input.metadata()?.len() != spec.bytes {
        return Err(invalid("download size mismatch"));
    }
    let mut digest = Sha256::new();
    let mut done = 0;
    let mut buffer = [0u8; 65536];
    loop {
        continuing(progress(SetupPhase::CheckingDownload, done, spec.bytes))?;
        let size = input.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        done += size as u64;
        if done > spec.bytes {
            return Err(invalid("download exceeded size limit"));
        }
        digest.update(&buffer[..size]);
    }
    if done != spec.bytes || format!("{:x}", digest.finalize()) != spec.sha256 {
        return Err(invalid("download integrity mismatch"));
    }
    // Keep extraction on the same user-selected storage volume as its download.
    let staged = tempfile::Builder::new().prefix("unpack-").tempdir_in(
        download
            .parent()
            .ok_or_else(|| invalid("missing download directory"))?,
    )?;
    match spec.format {
        DownloadFormat::File => {
            let path = staged.path().join(&manifest.files[0].path);
            fs::create_dir_all(path.parent().unwrap())?;
            let mut output = File::create(path)?;
            let mut input = File::open(download)?;
            let mut done = 0;
            loop {
                continuing(progress(SetupPhase::Extracting, done, spec.bytes))?;
                let size = input.read(&mut buffer)?;
                if size == 0 {
                    break;
                }
                done += size as u64;
                if done > spec.bytes {
                    return Err(invalid("download changed during preparation"));
                }
                output.write_all(&buffer[..size])?;
            }
        }
        DownloadFormat::TarBz2 => extract(manifest, spec, download, staged.path(), &mut progress)?,
    }
    store.install_with_progress(manifest, staged.path(), |done, total| {
        progress(SetupPhase::Installing, done, total)
    })
}

fn extract(
    manifest: &AssetManifest,
    spec: &DownloadSpec,
    download: &Path,
    destination: &Path,
    progress: &mut dyn FnMut(SetupPhase, u64, u64) -> bool,
) -> io::Result<()> {
    // Allow bounded upstream documentation/test audio overhead, never materialize
    // it. Bound the *decompressed stream*, including headers and skipped entries.
    let limit = manifest.files.iter().map(|file| file.bytes).sum::<u64>() + 64 * 1024 * 1024;
    let input = bzip2::read::BzDecoder::new(File::open(download)?);
    let input = LimitedReader {
        input,
        count: 0,
        limit,
        progress,
    };
    let mut archive = tar::Archive::new(input);
    let prefix = spec.strip_prefix.as_deref().unwrap();
    let mut seen = HashSet::new();
    let mut found = HashSet::new();
    // Raw headers prevent implicit allocation of attacker-supplied long-name or
    // PAX metadata. This catalog uses ordinary paths; other layouts need review.
    for (index, entry) in archive.entries()?.raw(true).enumerate() {
        if index >= 10_000 {
            return Err(invalid("too many archive entries"));
        }
        let mut entry = entry?;
        let name = String::from_utf8(entry.path_bytes().into_owned())
            .map_err(|_| invalid("non-UTF8 archive path"))?;
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err(invalid(
                "archive links or extended entries are not supported",
            ));
        }
        let name = if kind.is_dir() {
            name.strip_suffix('/').unwrap_or(&name)
        } else {
            &name
        };
        portable_path(name)?;
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(invalid("duplicate archive path"));
        }
        if name == prefix && kind.is_dir() {
            continue;
        }
        let relative = name
            .strip_prefix(prefix)
            .and_then(|s| s.strip_prefix('/'))
            .ok_or_else(|| invalid("archive path outside expected directory"))?;
        if kind.is_dir() {
            if entry.size() != 0 {
                return Err(invalid("nonempty archive directory"));
            }
            continue;
        }
        let Some(file) = manifest.files.iter().find(|file| file.path == relative) else {
            // Explicit draining keeps skipped data cancellable and bounded.
            io::copy(&mut entry, &mut io::sink())?;
            continue;
        };
        if entry.size() != file.bytes {
            return Err(invalid("archive file size mismatch"));
        }
        let path = destination.join(relative);
        fs::create_dir_all(path.parent().unwrap())?;
        let mut output = File::options().write(true).create_new(true).open(path)?;
        io::copy(&mut entry, &mut output)?;
        found.insert(relative.to_owned());
    }
    // Finish the decoder to check stream errors and bound trailing data too.
    io::copy(&mut archive.into_inner(), &mut io::sink())?;
    if found.len() != manifest.files.len() {
        return Err(invalid("archive is missing required files"));
    }
    Ok(())
}

struct LimitedReader<'a, R> {
    input: R,
    count: u64,
    limit: u64,
    progress: &'a mut dyn FnMut(SetupPhase, u64, u64) -> bool,
}
impl<R: Read> Read for LimitedReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        continuing((self.progress)(
            SetupPhase::Extracting,
            self.count,
            self.limit,
        ))?;
        let length = buffer.len().min(65536);
        let size = self.input.read(&mut buffer[..length])?;
        self.count += size as u64;
        if self.count > self.limit {
            return Err(invalid("decompressed archive exceeded limit"));
        }
        Ok(size)
    }
}

fn continuing(value: bool) -> io::Result<()> {
    // Read adapters (including io::copy/read_exact) retry Interrupted. A
    // cancellation here is terminal, not a request to retry a system call.
    if value {
        Ok(())
    } else {
        Err(io::Error::other("setup cancelled"))
    }
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Blocking entry point for the dedicated setup thread. Internally the HTTP
/// future is cancellable even while DNS/TLS/headers/body have not progressed.
/// No credentials, cookies, automatic retries, or HTTP content decoding.
pub fn download_https(
    url: &str,
    spec: &DownloadSpec,
    destination: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> io::Result<()> {
    let url = reqwest::Url::parse(url).map_err(|_| invalid("invalid download URL"))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid("download requires a public HTTPS URL"));
    }
    if spec.bytes == 0 || spec.bytes > 16 * 1024 * 1024 * 1024 {
        return Err(invalid("invalid download size"));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        let transfer = async {
            let client = reqwest::Client::builder()
                .https_only(true)
                .no_proxy()
                .no_gzip()
                .no_brotli()
                .no_deflate()
                .no_zstd()
                .referer(false)
                .redirect(reqwest::redirect::Policy::limited(5))
                .retry(reqwest::retry::never())
                .connect_timeout(Duration::from_secs(15))
                .read_timeout(Duration::from_secs(20))
                .timeout(Duration::from_secs(30 * 60))
                .user_agent(concat!("VoiceType/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(http_error)?;
            let mut response = client
                .get(url)
                .header("Accept-Encoding", "identity")
                .send()
                .await
                .map_err(http_error)?
                .error_for_status()
                .map_err(http_error)?;
            if response.status() != reqwest::StatusCode::OK
                || response
                    .content_length()
                    .is_some_and(|length| length != spec.bytes)
                || response
                    .headers()
                    .get("content-encoding")
                    .is_some_and(|value| value != "identity")
            {
                return Err(invalid("unexpected download response"));
            }
            let mut output = File::options()
                .write(true)
                .create_new(true)
                .open(destination)?;
            let mut done = 0;
            let mut hash = Sha256::new();
            progress(0, spec.bytes);
            while let Some(chunk) = response.chunk().await.map_err(http_error)? {
                for part in chunk.chunks(65536) {
                    if cancel.load(Ordering::Acquire) {
                        return Err(cancelled());
                    }
                    done += part.len() as u64;
                    if done > spec.bytes {
                        return Err(invalid("download exceeded size limit"));
                    }
                    output.write_all(part)?;
                    hash.update(part);
                    progress(done, spec.bytes);
                }
            }
            if done != spec.bytes || format!("{:x}", hash.finalize()) != spec.sha256 {
                return Err(invalid("download integrity mismatch"));
            }
            output.sync_all()?;
            Ok(())
        };
        let cancelled_event = async {
            loop {
                if cancel.load(Ordering::Acquire) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        tokio::select! {
            biased;
            _ = cancelled_event => Err(cancelled()),
            result = transfer => result,
        }
    });
    // Tokio's system DNS resolver may use blocking OS calls. Dropping a runtime
    // must not make a cancelled request wait indefinitely for that resolver.
    runtime.shutdown_timeout(Duration::from_millis(100));
    result
}

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "setup cancelled")
}
fn http_error(error: reqwest::Error) -> io::Error {
    // Do not surface signed redirect URLs, proxy credentials or provider data.
    io::Error::new(
        if error.is_timeout() {
            io::ErrorKind::TimedOut
        } else {
            io::ErrorKind::Other
        },
        "model download failed",
    )
}
