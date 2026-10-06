//! Read-only actual-runtime check. No frontend is attached and no Start is sent.
//! Usage: probe_owned_local BINARY MODEL_DIR VAD [EXPECTED_LIBRARY_DIRECTORY]
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{fs, path::PathBuf, time::Duration};
    use voicetype_app_core::runtime::{LocalRuntimePaths, OwnedLocal};
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 && args.len() != 4 {
        return Err(
            "usage: probe_owned_local BINARY MODEL_DIR VAD [EXPECTED_LIBRARY_DIRECTORY]".into(),
        );
    }
    let profile = tempfile::tempdir()?;
    let paths = LocalRuntimePaths {
        executable: PathBuf::from(&args[0]).canonicalize()?,
        model_dir: PathBuf::from(&args[1]).canonicalize()?,
        vad_model: PathBuf::from(&args[2]).canonicalize()?,
        profile: profile.path().to_owned(),
    };
    let mut runtime = OwnedLocal::start(&paths, Duration::from_secs(30))?;
    let pid = runtime.process_id().ok_or("missing owned process")?;
    let process = PathBuf::from(format!("/proc/{pid}"));
    let maps = fs::read_to_string(process.join("maps"))?;
    if !maps.contains("libsherpa-onnx-c-api.so") || !maps.contains("libonnxruntime.so") {
        return Err("expected native runtime libraries were not loaded".into());
    }
    if let Some(expected) = args.get(3) {
        let directory = PathBuf::from(expected).canonicalize()?;
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
        "bundled_libraries_verified":args.len()==4})
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This probe requires the Linux local-engine adapter.");
    std::process::exit(2);
}
