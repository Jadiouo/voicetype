use std::fs;
use voicetype_app_core::{
    assets::{AssetFile, AssetManifest},
    spelling_install::SpellingInstaller,
};

#[test]
fn spelling_install_verifies_bundled_bytes_and_preserves_user_files() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("resources");
    fs::create_dir_all(source.join("models")).unwrap();
    fs::create_dir_all(source.join("_internal")).unwrap();
    let executable = if cfg!(windows) {
        "voicetype-csc.exe"
    } else {
        "voicetype-csc"
    };
    let files = [
        executable,
        "models/model-int8-fused.onnx",
        "models/tokenizer.json",
        "_internal/libstdc++.so.6",
    ]
    .map(|path| {
        fs::write(source.join(path), b"abc").unwrap();
        AssetFile {
            path: path.into(),
            bytes: 3,
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
            executable: path == executable,
        }
    });
    let manifest = AssetManifest {
        schema_version: 1,
        id: "csc-runtime".into(),
        version: "fixture".into(),
        platform: "any".into(),
        source_url: "https://example.org/spelling".into(),
        license: "MIT".into(),
        files: files.to_vec(),
    };
    let profile = root.path().join("profile");
    fs::create_dir(&profile).unwrap();
    fs::write(profile.join("vocab.toml"), b"# keep my terms").unwrap();
    let installer = SpellingInstaller::new(profile.clone(), manifest).unwrap();
    let prepared = installer.prepare(&source).unwrap();
    assert!(prepared
        .executable
        .starts_with(profile.join("spelling-assets/versions")));
    assert_eq!(fs::read(&prepared.model).unwrap(), b"abc");
    assert_eq!(
        fs::read(profile.join("vocab.toml")).unwrap(),
        b"# keep my terms"
    );
    assert_eq!(prepared.threads, 4);
    // A changed copy cannot be activated, even if its sidecar claims otherwise.
    fs::write(&prepared.model, b"bad").unwrap();
    fs::write(source.join("models/model-int8-fused.onnx"), b"bad").unwrap();
    assert!(installer.prepare(&source).is_err());
}
