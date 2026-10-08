use crate::{
    assets::{portable_path, AssetManifest, AssetStore},
    setup::{download_https, install_download, DownloadSpec, SetupPhase},
};
use serde::Serialize;
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
};

#[derive(Clone)]
pub struct ModelBundle {
    pub manifest: AssetManifest,
    pub download: DownloadSpec,
}

/// External network boundary. Native fixtures may provide model bytes here;
/// web content cannot choose a source, manifest, URL or executable path.
pub trait ModelSource: Send + Sync {
    fn download(
        &self,
        url: &str,
        spec: &DownloadSpec,
        path: &Path,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64, u64),
    ) -> io::Result<()>;
}
struct HttpsSource;
impl ModelSource for HttpsSource {
    fn download(
        &self,
        url: &str,
        spec: &DownloadSpec,
        path: &Path,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64, u64),
    ) -> io::Result<()> {
        download_https(url, spec, path, cancel, progress)
    }
}

#[derive(Clone, Serialize)]
pub struct SetupStatus {
    pub phase: SetupPhase,
    pub busy: bool,
    pub cancel_requested: bool,
    pub bundle: Option<String>,
    pub completed_bytes: u64,
    pub total_bytes: u64,
    pub error: Option<String>,
}
impl Default for SetupStatus {
    fn default() -> Self {
        Self {
            phase: SetupPhase::NotChecked,
            busy: false,
            cancel_requested: false,
            bundle: None,
            completed_bytes: 0,
            total_bytes: 0,
            error: None,
        }
    }
}

struct Job {
    cancel: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}
#[derive(Default)]
struct Owner {
    job: Option<Job>,
    stopped: bool,
}
pub struct ModelSetup {
    root: PathBuf,
    bundles: Vec<ModelBundle>,
    source: Arc<dyn ModelSource>,
    state: Arc<Mutex<SetupStatus>>,
    owner: Mutex<Owner>,
}

impl ModelSetup {
    /// Opening the app does no network/disk preparation and starts no engine.
    pub fn new(root: PathBuf) -> io::Result<Self> {
        Self::with_source(root, bundled_models()?, Arc::new(HttpsSource))
    }

    /// Catalog and external transport are supplied by native code only.
    pub fn with_source(
        root: PathBuf,
        bundles: Vec<ModelBundle>,
        source: Arc<dyn ModelSource>,
    ) -> io::Result<Self> {
        if !root.is_absolute() || bundles.is_empty() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut ids = HashSet::new();
        for bundle in &bundles {
            bundle.download.validate(&bundle.manifest)?;
            portable_path(&bundle.manifest.id)?;
            if bundle.manifest.id.contains('/')
                || !ids.insert(bundle.manifest.id.to_ascii_lowercase())
            {
                return Err(io::ErrorKind::InvalidInput.into());
            }
        }
        Ok(Self {
            root,
            bundles,
            source,
            state: Arc::new(Mutex::new(SetupStatus::default())),
            owner: Mutex::default(),
        })
    }

    pub fn status(&self) -> SetupStatus {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn start(&self) -> io::Result<SetupStatus> {
        let mut owner = self
            .owner
            .lock()
            .map_err(|_| io::Error::other("setup worker failed"))?;
        if owner.stopped {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if self.status().busy {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if let Some(job) = owner.job.take() {
            job.thread
                .join()
                .map_err(|_| io::Error::other("setup worker failed"))?;
        }
        let root = self.root.clone();
        let bundles = self.bundles.clone();
        let source = self.source.clone();
        let state = self.state.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        *state.lock().unwrap() = SetupStatus {
            phase: SetupPhase::CheckingInstalled,
            busy: true,
            ..SetupStatus::default()
        };
        let result = thread::Builder::new()
            .name("voicetype-model-setup".into())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    prepare(&root, &bundles, source.as_ref(), &token, &state)
                }));
                let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
                state.busy = false;
                state.cancel_requested = false;
                state.phase = match outcome {
                    Ok(Ok(())) => SetupPhase::Installed,
                    Ok(Err(_)) if token.load(Ordering::Acquire) => SetupPhase::Cancelled,
                    _ => {
                        state.error = Some(
                            "模型準備未完成，請檢查網路與可用空間後重試。原有資料仍保留。".into(),
                        );
                        SetupPhase::Failed
                    }
                };
            });
        match result {
            Ok(thread) => owner.job = Some(Job { cancel, thread }),
            Err(error) => {
                let mut state = self.state.lock().unwrap();
                state.busy = false;
                state.phase = SetupPhase::Failed;
                return Err(error);
            }
        }
        Ok(self.status())
    }

    pub fn cancel(&self) -> SetupStatus {
        let owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.busy {
            if let Some(job) = &owner.job {
                job.cancel.store(true, Ordering::Release);
                state.cancel_requested = true;
            }
        }
        state.clone()
    }

    /// Cancel/join separately from the resident dictation worker.
    pub fn shutdown(&self) -> io::Result<()> {
        let mut owner = self
            .owner
            .lock()
            .map_err(|_| io::Error::other("setup worker failed"))?;
        owner.stopped = true;
        if let Some(job) = owner.job.take() {
            job.cancel.store(true, Ordering::Release);
            job.thread
                .join()
                .map_err(|_| io::Error::other("setup worker failed"))?;
        }
        Ok(())
    }
}
impl Drop for ModelSetup {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn prepare(
    root: &Path,
    bundles: &[ModelBundle],
    source: &dyn ModelSource,
    cancel: &AtomicBool,
    state: &Mutex<SetupStatus>,
) -> io::Result<()> {
    let mut directory = fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(root)?;
    if !fs::symlink_metadata(root)?.is_dir() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(root.join("setup.lock"))?;
    lock.try_lock()
        .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "setup already running"))?;
    for bundle in bundles {
        let mut update = |phase, done, total| {
            let mut status = state.lock().unwrap_or_else(|error| error.into_inner());
            status.phase = phase;
            status.bundle = Some(bundle.manifest.id.clone());
            status.completed_bytes = done;
            status.total_bytes = total;
            !cancel.load(Ordering::Acquire)
        };
        if !update(SetupPhase::CheckingInstalled, 0, 0) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let store = AssetStore::open(root.join(&bundle.manifest.id))?;
        match store.matching(&bundle.manifest, |done, total| {
            update(SetupPhase::CheckingInstalled, done, total)
        }) {
            Ok(Some(_)) => continue,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
            _ => {} // Missing/changed bytes are repaired in a new version.
        }
        if cancel.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let scratch = tempfile::Builder::new()
            .prefix("download-")
            .tempdir_in(root)?;
        let path = scratch.path().join("payload");
        update(SetupPhase::Downloading, 0, bundle.download.bytes);
        source.download(
            &bundle.manifest.source_url,
            &bundle.download,
            &path,
            cancel,
            &mut |done, total| {
                update(SetupPhase::Downloading, done, total);
            },
        )?;
        install_download(
            &store,
            &bundle.manifest,
            &bundle.download,
            &path,
            &mut update,
        )?;
    }
    Ok(())
}

pub fn bundled_models() -> io::Result<Vec<ModelBundle>> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Catalog {
        schema_version: u32,
        downloads: Vec<serde_json::Value>,
    }
    let entries: Catalog = serde_json::from_str(include_str!("../../assets/downloads.json"))
        .map_err(io::Error::other)?;
    if entries.schema_version != 1 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    entries
        .downloads
        .into_iter()
        .map(|mut value| {
            let name = value
                .as_object_mut()
                .and_then(|value| value.remove("manifest"));
            let manifest = match name.as_ref().and_then(|name| name.as_str()) {
                Some("nano-models.json") => include_str!("../../assets/nano-models.json"),
                Some("silero-vad.json") => include_str!("../../assets/silero-vad.json"),
                _ => return Err(io::ErrorKind::InvalidData.into()),
            };
            Ok(ModelBundle {
                manifest: serde_json::from_str(manifest).map_err(io::Error::other)?,
                download: serde_json::from_value(value).map_err(io::Error::other)?,
            })
        })
        .collect()
}
