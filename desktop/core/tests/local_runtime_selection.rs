use std::{
    fs,
    path::{Path, PathBuf},
};
use voicetype_app_core::{
    assets::{AssetManifest, AssetStore},
    local_install::LocalInstaller,
};

const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
const DEF: &str = "cb8379ac2098aa165029e3938a51da0bcecfc008fd6795f401178647f96c5b34";

fn executable() -> &'static str {
    if cfg!(windows) {
        "bin/voicetyped.exe"
    } else {
        "bin/voicetyped"
    }
}

fn manifest(id: &str, version: &str, file: &str, hash: &str, executable: bool) -> AssetManifest {
    serde_json::from_value(serde_json::json!({
        "schema_version": 1, "id": id, "version": version, "platform": "any",
        "source_url": "https://example.invalid/verified", "license": "MIT",
        "files": [{ "path": file, "bytes": 3, "sha256": hash, "executable": executable }]
    }))
    .unwrap()
}

fn source(root: &Path, name: &str, file: &str, bytes: &[u8]) -> PathBuf {
    let source = root.join(name);
    let destination = source.join(file);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(destination, bytes).unwrap();
    source
}

fn fixture(root: &Path) -> (PathBuf, Vec<AssetManifest>) {
    let profile = root.join("profile");
    let mut models = Vec::new();
    for (id, file) in [
        ("funasr-nano-int8", "model.onnx"),
        ("silero-vad", "silero-v5.0.onnx"),
    ] {
        let model = manifest(id, "1", file, ABC, false);
        let source = source(root, id, file, b"abc");
        AssetStore::open(profile.join("model-assets").join(id))
            .unwrap()
            .install(&model, &source)
            .unwrap();
        models.push(model);
    }
    (profile, models)
}

#[test]
fn rollback_selection_survives_restart_and_repair_returns_to_current_bundle_without_touching_user_data(
) {
    let root = tempfile::tempdir().unwrap();
    let (profile, models) = fixture(root.path());
    let old = manifest("nano-runtime", "1", executable(), ABC, true);
    let current = manifest("nano-runtime", "2", executable(), DEF, true);
    let old_source = source(root.path(), "old", executable(), b"abc");
    let current_source = source(root.path(), "current", executable(), b"def");
    let first = LocalInstaller::new(profile.clone(), old, models.clone()).unwrap();
    let old_paths = first.prepare(&old_source, |_, _| true).unwrap();
    let installer = LocalInstaller::new(profile.clone(), current.clone(), models.clone()).unwrap();
    let new_paths = installer.prepare(&current_source, |_, _| true).unwrap();
    assert_ne!(old_paths.executable, new_paths.executable);
    for (path, bytes) in [
        ("desktop.json", b"settings".as_slice()),
        ("vocab.toml", b"vocabulary"),
        ("local-profile/data/learning.json", b"learning"),
        ("review.json", b"review opt-in"),
        ("review/clips/sample.wav", b"sample"),
    ] {
        let path = profile.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    assert_eq!(
        installer.rollback(|_, _| true).unwrap().executable,
        old_paths.executable
    );
    let restarted = LocalInstaller::new(profile.clone(), current, models).unwrap();
    assert_eq!(
        restarted
            .prepare(&current_source, |_, _| true)
            .unwrap()
            .executable,
        old_paths.executable
    );
    let repaired = restarted.repair(&current_source, |_, _| true).unwrap();
    assert_ne!(repaired.executable, old_paths.executable);
    assert_eq!(fs::read(repaired.executable).unwrap(), b"def");
    for (path, bytes) in [
        ("desktop.json", b"settings".as_slice()),
        ("vocab.toml", b"vocabulary"),
        ("local-profile/data/learning.json", b"learning"),
        ("review.json", b"review opt-in"),
        ("review/clips/sample.wav", b"sample"),
    ] {
        assert_eq!(fs::read(profile.join(path)).unwrap(), bytes);
    }
}

#[test]
fn corrupted_rollback_target_fails_closed_until_explicit_repair() {
    let root = tempfile::tempdir().unwrap();
    let (profile, models) = fixture(root.path());
    let old_source = source(root.path(), "old", executable(), b"abc");
    let current_source = source(root.path(), "current", executable(), b"def");
    let old = LocalInstaller::new(
        profile.clone(),
        manifest("nano-runtime", "1", executable(), ABC, true),
        models.clone(),
    )
    .unwrap();
    let old_paths = old.prepare(&old_source, |_, _| true).unwrap();
    let current_manifest = manifest("nano-runtime", "2", executable(), DEF, true);
    let current =
        LocalInstaller::new(profile.clone(), current_manifest.clone(), models.clone()).unwrap();
    current.prepare(&current_source, |_, _| true).unwrap();
    current.rollback(|_, _| true).unwrap();
    fs::write(old_paths.executable, b"bad").unwrap();
    let restarted = LocalInstaller::new(profile.clone(), current_manifest, models).unwrap();
    assert!(restarted.prepare(&current_source, |_, _| true).is_err());
    assert!(restarted.repair(&current_source, |_, _| true).is_ok());
}

#[test]
fn rollback_rejects_a_verified_asset_of_the_wrong_product_before_switching() {
    let root = tempfile::tempdir().unwrap();
    let (profile, models) = fixture(root.path());
    let store = AssetStore::open(profile.join("runtime-assets")).unwrap();
    let other = manifest("unrelated-asset", "1", "other.bin", ABC, false);
    store
        .install(&other, &source(root.path(), "other", "other.bin", b"abc"))
        .unwrap();
    let current_source = source(root.path(), "current", executable(), b"def");
    let installer = LocalInstaller::new(
        profile.clone(),
        manifest("nano-runtime", "2", executable(), DEF, true),
        models,
    )
    .unwrap();
    let selected = installer.prepare(&current_source, |_, _| true).unwrap();
    assert!(installer.rollback(|_, _| true).is_err());
    assert_eq!(
        store.active().unwrap().unwrap().join(executable()),
        selected.executable
    );
}

#[test]
fn lost_selection_with_existing_versions_requires_explicit_repair() {
    let root = tempfile::tempdir().unwrap();
    let (profile, models) = fixture(root.path());
    let old_source = source(root.path(), "old", executable(), b"abc");
    let current_source = source(root.path(), "current", executable(), b"def");
    LocalInstaller::new(
        profile.clone(),
        manifest("nano-runtime", "1", executable(), ABC, true),
        models.clone(),
    )
    .unwrap()
    .prepare(&old_source, |_, _| true)
    .unwrap();
    let current = LocalInstaller::new(
        profile.clone(),
        manifest("nano-runtime", "2", executable(), DEF, true),
        models,
    )
    .unwrap();
    current.prepare(&current_source, |_, _| true).unwrap();
    current.rollback(|_, _| true).unwrap();
    fs::remove_file(profile.join("runtime-assets/active.json")).unwrap();
    assert!(current.prepare(&current_source, |_, _| true).is_err());
    assert_eq!(
        fs::read(
            current
                .repair(&current_source, |_, _| true)
                .unwrap()
                .executable
        )
        .unwrap(),
        b"def"
    );
}

#[test]
fn corrupt_selection_requires_explicit_repair_and_preserves_personal_files() {
    let root = tempfile::tempdir().unwrap();
    let (profile, models) = fixture(root.path());
    let current_source = source(root.path(), "current", executable(), b"def");
    let installer = LocalInstaller::new(
        profile.clone(),
        manifest("nano-runtime", "2", executable(), DEF, true),
        models,
    )
    .unwrap();
    installer.prepare(&current_source, |_, _| true).unwrap();
    let private_files = [
        ("desktop.json", b"settings".as_slice()),
        ("vocab.toml", b"vocabulary"),
        ("local-profile/data/learning.json", b"learning"),
        ("review.json", b"review opt-in"),
        ("review/clips/sample.wav", b"sample"),
    ];
    for (name, contents) in private_files {
        let path = profile.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    fs::write(profile.join("runtime-assets/active.json"), b"{corrupt").unwrap();
    assert!(installer.prepare(&current_source, |_, _| true).is_err());
    let repaired = installer.repair(&current_source, |_, _| true).unwrap();
    assert_eq!(fs::read(repaired.executable).unwrap(), b"def");
    for (name, contents) in private_files {
        assert_eq!(fs::read(profile.join(name)).unwrap(), contents);
    }
}
