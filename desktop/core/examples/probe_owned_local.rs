//! Read-only actual-runtime check. No frontend is attached and no Start is sent.
//! Usage: cargo run -p voicetype-app-core --example probe_owned_local -- BINARY MODEL_DIR VAD
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{fs, path::PathBuf, time::Duration};
    use voicetype_app_core::runtime::{LocalRuntimePaths, OwnedLocal};
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: probe_owned_local BINARY MODEL_DIR VAD".into());
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
    let maps = fs::read_to_string(process.join("maps"))?.to_lowercase();
    if !maps.contains("libsherpa-onnx-c-api.so") || !maps.contains("libonnxruntime.so") {
        return Err("expected native runtime libraries were not loaded".into());
    }
    if ["libcuda", "libcudnn", "libnvinfer"]
        .iter()
        .any(|name| maps.contains(name))
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
        "gpu_libraries_loaded":false,"start_sent":false,"owned_child_reaped":true})
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This probe requires the Linux local-engine adapter.");
    std::process::exit(2);
}
