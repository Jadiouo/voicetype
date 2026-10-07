//! Read-only actual-runtime check. No frontend is attached and no Start is sent.
//! Usage: probe_owned_local BINARY MODEL_DIR VAD [EXPECTED_LIBRARY_DIRECTORY]
//! Or: probe_owned_local --package BUNDLE MODEL_DIR VAD
//! Package mode uses a disposable store and the app's real installation path.
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{fs, path::PathBuf, time::Duration};
    use voicetype_app_core::runtime::{LocalRuntimePaths, OwnedLocal};
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    let packaged = args.first().is_some_and(|value| value == "--package");
    if packaged {
        args.remove(0);
    }
    if args.len() != 3 && args.len() != 4 {
        return Err(
            "usage: probe_owned_local BINARY MODEL_DIR VAD [EXPECTED_LIBRARY_DIRECTORY]".into(),
        );
    }
    let profile = tempfile::tempdir()?;
    let mut paths = LocalRuntimePaths {
        executable: PathBuf::from(&args[0]).canonicalize()?,
        model_dir: PathBuf::from(&args[1]).canonicalize()?,
        vad_model: PathBuf::from(&args[2]).canonicalize()?,
        profile: profile.path().to_owned(),
    };
    let mut expected_libraries = args.get(3).map(PathBuf::from);
    if packaged {
        use voicetype_app_core::{
            assets::AssetStore, local_install::LocalInstaller, setup::bundled_models,
        };
        let resource_dir = paths.executable.clone();
        let config = profile.path().join("config");
        let models = bundled_models()?;
        AssetStore::open(config.join("model-assets").join(&models[0].manifest.id))?
            .install(&models[0].manifest, &paths.model_dir)?;
        let vad_source = tempfile::tempdir()?;
        fs::copy(&paths.vad_model, vad_source.path().join("silero-v5.0.onnx"))?;
        AssetStore::open(config.join("model-assets").join(&models[1].manifest.id))?
            .install(&models[1].manifest, vad_source.path())?;
        // Explicit CLI build-verification input, not an app runtime trust source.
        let manifest = serde_json::from_slice(&fs::read(resource_dir.join("manifest.json"))?)?;
        paths = LocalInstaller::bundled(config, manifest)?.prepare(&resource_dir, |_, _| true)?;
        expected_libraries = Some(
            paths
                .executable
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("lib"),
        );
    }
    let mut runtime = OwnedLocal::start(&paths, Duration::from_secs(30))?;
    let pid = runtime.process_id().ok_or("missing owned process")?;
    let process = PathBuf::from(format!("/proc/{pid}"));
    let maps = fs::read_to_string(process.join("maps"))?;
    if !maps.contains("libsherpa-onnx-c-api.so") || !maps.contains("libonnxruntime.so") {
        return Err("expected native runtime libraries were not loaded".into());
    }
    if let Some(expected) = &expected_libraries {
        let directory = expected.canonicalize()?;
        for library in ["libsherpa-onnx-c-api.so", "libonnxruntime.so"] {
            let suffix = format!(" {}", directory.join(library).display());
            if !maps.lines().any(|line| line.ends_with(&suffix)) {
                return Err("native library was loaded outside the expected bundle".into());
            }
        }
    }
    if ["libcuda", "libcudnn", "libnvinfer"]
        .iter()
        .any(|name| maps.to_lowercase().contains(name))
    {
        return Err("unexpected GPU runtime library".into());
    }
    runtime.shutdown()?;
    if process.exists() || runtime.process_id().is_some() {
        return Err("owned child was not reaped".into());
    }
    println!(
        "{}",
        serde_json::json!({"native_libraries_loaded":true,
        "gpu_libraries_loaded":false,"start_sent":false,"owned_child_reaped":true,
        "bundled_libraries_verified":expected_libraries.is_some(),
        "verified_app_installation_path":packaged})
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This probe requires the Linux local-engine adapter.");
    std::process::exit(2);
}
