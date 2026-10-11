//! Prepare trusted packaged runtime bytes and already-installed models off the
//! dictation thread. No network, process launch, microphone or service migration.
#[cfg(target_os = "linux")]
use crate::runtime::LocalRuntimePaths;
#[cfg(windows)]
use crate::windows_engine::EnginePaths as LocalRuntimePaths;
use crate::{
    assets::{AssetManifest, AssetStore},
    setup::bundled_models,
};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub struct LocalInstaller {
    root: PathBuf,
    runtime: AssetManifest,
    models: Vec<AssetManifest>,
}

impl LocalInstaller {
    /// Runtime manifest must be embedded by the app build, not read from the
    /// resource directory at runtime. Models use the reviewed compiled catalog.
    pub fn bundled(root: PathBuf, runtime: AssetManifest) -> io::Result<Self> {
        Self::new(
            root,
            runtime,
            bundled_models()?.into_iter().map(|m| m.manifest).collect(),
        )
    }

    /// Native catalog boundary, also usable with small pinned model fixtures.
    /// The webview is never allowed to supply any of these arguments.
    pub fn new(
        root: PathBuf,
        runtime: AssetManifest,
        models: Vec<AssetManifest>,
    ) -> io::Result<Self> {
        runtime.validate()?;
        if !root.is_absolute()
            || runtime.id != "nano-runtime"
            || !runtime.files.iter().any(|f| {
                f.path
                    == if cfg!(windows) {
                        "bin/voicetyped.exe"
                    } else {
                        "bin/voicetyped"
                    }
                    && f.executable
            })
            || models.len() != 2
            || models[0].id != "funasr-nano-int8"
            || models[1].id != "silero-vad"
            || !models[1].files.iter().any(|f| f.path == "silero-v5.0.onnx")
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        for model in &models {
            model.validate()?;
            if model.files.iter().any(|f| f.executable) {
                return Err(io::ErrorKind::InvalidInput.into());
            }
        }
        Ok(Self {
            root,
            runtime,
            models,
        })
    }

    /// Full hash verification belongs to explicit setup, never the warm record
    /// path. Returned directories are immutable version directories, so a later
    /// installer pointer update cannot silently swap a running engine's files.
    pub fn prepare(
        &self,
        source: &Path,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> io::Result<LocalRuntimePaths> {
        let installed_models = self.checked_models(&mut progress)?;
        let store = AssetStore::open(self.root.join("runtime-assets"))?;
        let runtime = match store.selected_rollback_with_progress(&mut progress)? {
            Some(selected) => {
                self.validate_runtime(&selected)?;
                selected
            }
            None => store.install_with_progress(&self.runtime, source, &mut progress)?,
        };
        Ok(self.paths(runtime, installed_models))
    }

    /// Explicit repair selects the currently bundled, hash-pinned runtime.
    /// It does not erase configuration, vocabulary, learning data or reviews.
    pub fn repair(
        &self,
        source: &Path,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> io::Result<LocalRuntimePaths> {
        let installed_models = self.checked_models(&mut progress)?;
        let runtime = AssetStore::open(self.root.join("runtime-assets"))?.repair_with_progress(
            &self.runtime,
            source,
            &mut progress,
        )?;
        Ok(self.paths(runtime, installed_models))
    }

    /// Select a previously installed and fully verified runtime. The selection
    /// is persisted with the activation pointer, so a restart cannot silently
    /// replace it with a newer bundle. No running engine may call this API.
    pub fn rollback(
        &self,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> io::Result<LocalRuntimePaths> {
        let installed_models = self.checked_models(&mut progress)?;
        let runtime = AssetStore::open(self.root.join("runtime-assets"))?
            .rollback_with_validation(|path| self.validate_runtime(path))?;
        Ok(self.paths(runtime, installed_models))
    }

    fn checked_models(
        &self,
        progress: &mut impl FnMut(u64, u64) -> bool,
    ) -> io::Result<Vec<PathBuf>> {
        let mut installed_models = Vec::new();
        for model in &self.models {
            let root = self.root.join("model-assets").join(&model.id);
            // Opening a missing store would create directories; an unavailable
            // model should simply request explicit model setup.
            if !root.is_dir() {
                return Err(io::ErrorKind::NotFound.into());
            }
            let installed = AssetStore::open(root)?
                .matching(model, &mut *progress)?
                .ok_or(io::ErrorKind::NotFound)?;
            installed_models.push(installed);
        }
        Ok(installed_models)
    }

    fn validate_runtime(&self, root: &Path) -> io::Result<()> {
        let receipt: AssetManifest = serde_json::from_slice(&fs::read(root.join("manifest.json"))?)
            .map_err(io::Error::other)?;
        receipt.validate()?;
        let executable = if cfg!(windows) {
            "bin/voicetyped.exe"
        } else {
            "bin/voicetyped"
        };
        if receipt.id != self.runtime.id
            || receipt.platform != self.runtime.platform
            || !receipt
                .files
                .iter()
                .any(|file| file.path == executable && file.executable)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "selected runtime is incompatible",
            ));
        }
        Ok(())
    }

    fn paths(&self, runtime: PathBuf, installed_models: Vec<PathBuf>) -> LocalRuntimePaths {
        LocalRuntimePaths {
            executable: runtime.join(if cfg!(windows) {
                "bin/voicetyped.exe"
            } else {
                "bin/voicetyped"
            }),
            model_dir: installed_models[0].clone(),
            vad_model: installed_models[1].join("silero-v5.0.onnx"),
            profile: self.root.join("local-profile"),
            vocabulary: self.root.join("vocab.toml"),
            review_config: self.root.join("review.json"),
            review_root: self.root.join("review"),
            spelling: None,
        }
    }
}
