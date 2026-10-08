fn main() {
    // The runtime catalog is a build artifact from the reviewed source-build
    // pipeline. Embed its bytes; never trust a replaceable installed sidecar.
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    if target == "linux" || target == "windows" {
        use sha2::{Digest, Sha256};
        let bundles: &[&str] = if target == "linux" {
            &["runtime", "input", "spelling"]
        } else {
            &["opencc", "spelling"]
        };
        for name in bundles {
            let root = std::path::PathBuf::from("../target").join(name);
            let manifest = root.join("manifest.json");
            println!("cargo:rerun-if-changed={}", manifest.display());
            let data = std::fs::read(&manifest)
                .expect("First run the documented Linux native/runtime build steps");
            let catalog: serde_json::Value =
                serde_json::from_slice(&data).expect("Invalid runtime catalog");
            for entry in catalog["files"].as_array().expect("Missing runtime files") {
                let relative = entry["path"].as_str().expect("Invalid runtime member");
                assert!(std::path::Path::new(relative)
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))));
                let path = root.join(relative);
                println!("cargo:rerun-if-changed={}", path.display());
                assert!(
                    std::fs::symlink_metadata(&path).unwrap().is_file(),
                    "Runtime member is not a regular file"
                );
                let bytes = std::fs::read(&path).expect("Missing runtime member");
                assert_eq!(Some(bytes.len() as u64), entry["bytes"].as_u64());
                assert_eq!(
                    Some(format!("{:x}", Sha256::digest(&bytes)).as_str()),
                    entry["sha256"].as_str(),
                    "Runtime member hash mismatch"
                );
            }
            std::fs::write(out.join(format!("{name}-catalog.json")), data).unwrap();
        }
    } else {
        std::fs::write(out.join("runtime-catalog.json"), b"null").unwrap();
        std::fs::write(out.join("input-catalog.json"), b"null").unwrap();
    }
    tauri_build::build();
}
