//! CPU-only setup verification. Never starts a model, microphone or provider.
use std::{io, path::PathBuf, sync::atomic::AtomicBool};
use voicetype_app_core::{
    assets::AssetStore,
    setup::{bundled_models, download_https, install_download, ModelSetup, SetupPhase},
};

fn main() -> io::Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let root = tempfile::tempdir()?;
    if mode == "all-download" {
        let setup = ModelSetup::new(root.path().join("models"))?;
        setup.start()?;
        let mut phase = None;
        loop {
            let status = setup.status();
            if phase != Some(status.phase) {
                eprintln!("{:?}", status.phase);
                phase = Some(status.phase);
            }
            if !status.busy {
                if status.phase != SetupPhase::Installed {
                    return Err(io::Error::other("model setup failed"));
                }
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        setup.shutdown()?;
        for bundle in bundled_models()? {
            let store = AssetStore::open(root.path().join("models").join(&bundle.manifest.id))?;
            if store.matching(&bundle.manifest, |_, _| true)?.is_none() {
                return Err(io::ErrorKind::NotFound.into());
            }
        }
        println!(
            "{}",
            serde_json::json!({"bundles":2,"downloaded_installed_and_reopened":true,
            "model_executed":false,"disposable_install":true})
        );
        return Ok(());
    }
    let store = AssetStore::open(root.path().join("installed"))?;
    let mut catalog = bundled_models()?;
    let (bundle, download) = match mode.as_str() {
        "nano-cache" => {
            let path = std::env::args_os()
                .nth(2)
                .ok_or(io::ErrorKind::InvalidInput)?;
            (catalog.remove(0), PathBuf::from(path))
        }
        "vad-download" => {
            let bundle = catalog.remove(1);
            let path = root.path().join("payload");
            download_https(
                &bundle.manifest.source_url,
                &bundle.download,
                &path,
                &AtomicBool::new(false),
                &mut |_, _| {},
            )?;
            (bundle, path)
        }
        _ => {
            return Err(io::Error::other(
                "use all-download, nano-cache ARCHIVE or vad-download",
            ))
        }
    };
    let installed = install_download(
        &store,
        &bundle.manifest,
        &bundle.download,
        &download,
        |_, _, _| true,
    )?;
    if store.matching(&bundle.manifest, |_, _| true)?.as_ref() != Some(&installed) {
        return Err(io::Error::other("installed version did not match catalog"));
    }
    println!(
        "{}",
        serde_json::json!({"asset":bundle.manifest.id,"files":bundle.manifest.files.len(),
        "installed_and_verified":true,"model_executed":false,"disposable_install":true})
    );
    Ok(())
}
