#![cfg(target_os = "linux")]
use std::fs;
use voicetype_app_core::{
    assets::{AssetManifest, AssetStore},
    local_install::LocalInstaller,
};

fn runtime_manifest() -> AssetManifest {
    serde_json::from_str(r#"{
      "schema_version":1,"id":"nano-runtime","version":"fixture","platform":"linux-x86_64",
      "source_url":"https://example.org/runtime","license":"MIT",
      "files":[{"path":"bin/voicetyped","bytes":3,
      "sha256":"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad","executable":true}]
    }"#).unwrap()
}

#[test]
fn missing_models_cannot_prepare_or_publish_a_runtime() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("resources");
    fs::create_dir_all(source.join("bin")).unwrap();
    fs::write(source.join("bin/voicetyped"), b"abc").unwrap();
    let profile = root.path().join("profile");
    let installer = LocalInstaller::bundled(profile.clone(), runtime_manifest()).unwrap();
    assert_eq!(
        installer
            .prepare(&source, |_, _| true)
            .err()
            .unwrap()
            .kind(),
        std::io::ErrorKind::NotFound
    );
    assert!(!profile.join("runtime-assets").exists());
}

fn fixture_models(profile: &std::path::Path, source: &std::path::Path) -> Vec<AssetManifest> {
    [
        ("funasr-nano-int8", "model.onnx"),
        ("silero-vad", "silero-v5.0.onnx"),
    ]
    .into_iter()
    .map(|(id, file)| {
        let mut manifest = runtime_manifest();
        manifest.id = id.into();
        manifest.files[0].path = file.into();
        manifest.files[0].executable = false;
        fs::write(source.join(file), b"abc").unwrap();
        AssetStore::open(profile.join("model-assets").join(id))
            .unwrap()
            .install(&manifest, source)
            .unwrap();
        manifest
    })
    .collect()
}

#[test]
fn prepared_paths_use_verified_private_versions_and_preserve_personal_files() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("resources");
    fs::create_dir_all(source.join("bin")).unwrap();
    fs::write(source.join("bin/voicetyped"), b"abc").unwrap();
    let profile = root.path().join("profile");
    let models = fixture_models(&profile, &source);
    fs::write(profile.join("vocabulary.json"), b"personal data").unwrap();
    let installer = LocalInstaller::new(profile.clone(), runtime_manifest(), models).unwrap();
    let paths = installer.prepare(&source, |_, _| true).unwrap();
    assert!(paths
        .executable
        .starts_with(profile.join("runtime-assets/versions")));
    assert_eq!(fs::read(&paths.executable).unwrap(), b"abc");
    assert_eq!(
        fs::read(paths.model_dir.join("model.onnx")).unwrap(),
        b"abc"
    );
    assert_eq!(fs::read(paths.vad_model).unwrap(), b"abc");
    assert!(
        !paths.profile.exists(),
        "Preparation started the engine profile"
    );
    assert_eq!(
        fs::read(profile.join("vocabulary.json")).unwrap(),
        b"personal data"
    );
    // A subsequent setup reuses verified installed bytes even if resources are
    // no longer available; it must not change the engine's immutable directory.
    fs::remove_file(source.join("bin/voicetyped")).unwrap();
    assert_eq!(
        installer.prepare(&source, |_, _| true).unwrap().executable,
        paths.executable
    );
}

#[test]
fn modified_payload_cannot_authorize_itself_with_a_replacement_manifest() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("resources");
    fs::create_dir_all(source.join("bin")).unwrap();
    fs::write(source.join("bin/voicetyped"), b"bad").unwrap();
    let profile = root.path().join("profile");
    let models = fixture_models(&profile, &source);
    let mut replacement = runtime_manifest();
    replacement.files[0].sha256 =
        "2f05d4b689d270cafb02285f35f44866f7dc8a2d368a3f9d1124373eeab31fb1".into();
    fs::write(
        source.join("manifest.json"),
        serde_json::to_vec(&replacement).unwrap(),
    )
    .unwrap();
    let installer = LocalInstaller::new(profile.clone(), runtime_manifest(), models).unwrap();
    assert!(installer.prepare(&source, |_, _| true).is_err());
    assert!(AssetStore::open(profile.join("runtime-assets"))
        .unwrap()
        .active()
        .unwrap()
        .is_none());
}

#[test]
fn cancelled_verification_does_not_publish_a_runtime() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("resources");
    fs::create_dir_all(&source).unwrap();
    let profile = root.path().join("profile");
    let models = fixture_models(&profile, &source);
    let installer = LocalInstaller::new(profile.clone(), runtime_manifest(), models).unwrap();
    assert_eq!(
        installer
            .prepare(&source, |_, _| false)
            .err()
            .unwrap()
            .kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(!profile.join("runtime-assets").exists());
}
