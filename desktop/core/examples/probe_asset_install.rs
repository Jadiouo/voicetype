//! Copies reviewed local assets into a disposable installation, then verifies
//! the committed version through the public store API. No model is executed.
use std::{fs, path::PathBuf};
use voicetype_app_core::assets::{AssetManifest, AssetStore};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: probe_asset_install MANIFEST SOURCE_DIRECTORY".into());
    }
    let manifest: AssetManifest = serde_json::from_slice(&fs::read(&args[0])?)?;
    let profile = tempfile::tempdir()?;
    let store = AssetStore::open(profile.path().join("assets"))?;
    let installed = store.install(&manifest, &PathBuf::from(&args[1]))?;
    if store.active()?.as_ref() != Some(&installed) {
        return Err("installed version was not active".into());
    }
    println!(
        "{}",
        serde_json::json!({"id":manifest.id,"version":manifest.version,
        "verified_files":manifest.files.len(),"model_executed":false,"disposable_install":true})
    );
    Ok(())
}
