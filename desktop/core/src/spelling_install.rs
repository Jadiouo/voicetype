//! Install the compiled-in CPU spelling catalog without loading a model.
use crate::assets::{AssetManifest, AssetStore};
use std::{
    io,
    path::{Path, PathBuf},
};
use voicetype_text::spelling::SpellingPaths;

pub struct SpellingInstaller {
    root: PathBuf,
    manifest: AssetManifest,
}
impl SpellingInstaller {
    pub fn new(root: PathBuf, manifest: AssetManifest) -> io::Result<Self> {
        manifest.validate()?;
        if !root.is_absolute() || manifest.id != "csc-runtime" {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let this = Self { root, manifest };
        this.paths(Path::new("/validation"))?;
        Ok(this)
    }

    pub fn prepare(&self, source: &Path) -> io::Result<SpellingPaths> {
        let version =
            AssetStore::open(self.root.join("spelling-assets"))?.install(&self.manifest, source)?;
        self.paths(&version)
    }

    fn paths(&self, root: &Path) -> io::Result<SpellingPaths> {
        let executable = if cfg!(windows) {
            "voicetype-csc.exe"
        } else {
            "voicetype-csc"
        };
        let member = |name: &str, executable: bool| {
            self.manifest
                .files
                .iter()
                .find(|f| f.path == name && f.executable == executable)
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))
        };
        member(executable, true)?;
        let model = member("models/model-int8-fused.onnx", false)?;
        let tokenizer = member("models/tokenizer.json", false)?;
        Ok(SpellingPaths {
            executable: root.join(executable),
            model: root.join(&model.path),
            tokenizer: root.join(&tokenizer.path),
            model_sha256: model.sha256.clone(),
            tokenizer_sha256: tokenizer.sha256.clone(),
            threads: 4,
        })
    }
}
