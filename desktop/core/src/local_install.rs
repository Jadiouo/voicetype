//! Prepare trusted packaged runtime bytes and already-installed models off the
//! dictation thread. No network, process launch, microphone or service migration.
use crate::{
    assets::{AssetManifest, AssetStore},
    runtime::LocalRuntimePaths,
    setup::bundled_models,
};
use std::{
    io,
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
            || !runtime
                .files
                .iter()
                .any(|f| f.path == "bin/voicetyped" && f.executable)
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
        let mut installed_models = Vec::new();
        for model in &self.models {
            let root = self.root.join("model-assets").join(&model.id);
            // Opening a missing store would create directories; an unavailable
            // model should simply request explicit model setup.
            if !root.is_dir() {
                return Err(io::ErrorKind::NotFound.into());
            }
            let installed = AssetStore::open(root)?
                .matching(model, &mut progress)?
                .ok_or(io::ErrorKind::NotFound)?;
            installed_models.push(installed);
        }
        let runtime = AssetStore::open(self.root.join("runtime-assets"))?.install_with_progress(
            &self.runtime,
            source,
            &mut progress,
        )?;
        Ok(LocalRuntimePaths {
            executable: runtime.join("bin/voicetyped"),
            model_dir: installed_models[0].clone(),
            vad_model: installed_models[1].join("silero-v5.0.onnx"),
            profile: self.root.join("local-profile"),
            vocabulary: self.root.join("vocab.toml"),
            review_config: self.root.join("review.json"),
            review_root: self.root.join("review"),
        })
    }
}
