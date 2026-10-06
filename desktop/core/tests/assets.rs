use std::{fs, path::Path};
use voicetype_app_core::assets::{AssetManifest, AssetStore};

fn manifest(version: &str, size: u64, sha: &str) -> AssetManifest {
    serde_json::from_value(serde_json::json!({
        "schema_version":1,"id":"fixture-model","version":version,"platform":"any",
        "source_url":"https://example.invalid/model","license":"MIT",
        "files":[{"path":"model.onnx","bytes":size,"sha256":sha,"executable":false}]
    }))
    .unwrap()
}

fn source(root: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = root.join(name);
    fs::create_dir(&path).unwrap();
    fs::write(path.join("model.onnx"), bytes).unwrap();
    path
}

#[test]
fn a_corrupt_update_preserves_the_working_install_and_personal_data() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("vocabulary.json"), b"personal vocabulary").unwrap();
    let store = AssetStore::open(root.path().join("assets")).unwrap();
    let first = manifest(
        "1",
        11,
        "c6ce303f2afd029639c46178cc233a099f531a566a8583e12699f259dc666fd5",
    );
    let second = manifest(
        "2",
        12,
        "fcda1814c7c9162f713b046f768a20185627ab3db15cf717079da879fc894d23",
    );
    let installed = store
        .install(&first, &source(root.path(), "old", b"first-model"))
        .unwrap();
    assert_eq!(
        fs::read(installed.join("model.onnx")).unwrap(),
        b"first-model"
    );
    assert!(store
        .install(&second, &source(root.path(), "corrupt", b"broken-model"))
        .is_err());
    let reopened = AssetStore::open(root.path().join("assets")).unwrap();
    assert_eq!(reopened.active().unwrap().unwrap(), installed);
    assert_eq!(
        fs::read(installed.join("model.onnx")).unwrap(),
        b"first-model"
    );
    assert_eq!(
        fs::read(root.path().join("vocabulary.json")).unwrap(),
        b"personal vocabulary"
    );
}

#[test]
fn an_upgrade_and_rollback_keep_both_complete_versions() {
    let root = tempfile::tempdir().unwrap();
    let store = AssetStore::open(root.path().join("assets")).unwrap();
    let first = manifest(
        "1",
        11,
        "c6ce303f2afd029639c46178cc233a099f531a566a8583e12699f259dc666fd5",
    );
    let second = manifest(
        "2",
        12,
        "fcda1814c7c9162f713b046f768a20185627ab3db15cf717079da879fc894d23",
    );
    let old = store
        .install(&first, &source(root.path(), "old", b"first-model"))
        .unwrap();
    let new = store
        .install(&second, &source(root.path(), "new", b"second-model"))
        .unwrap();
    assert_eq!(store.active().unwrap().unwrap(), new);
    assert_eq!(store.rollback().unwrap(), old);
    assert_eq!(store.active().unwrap().unwrap(), old);
    assert_eq!(fs::read(new.join("model.onnx")).unwrap(), b"second-model");
    assert_eq!(store.rollback().unwrap(), new);
    fs::write(old.join("model.onnx"), b"wrong-model").unwrap();
    assert!(
        store.rollback().is_err(),
        "corrupted rollback must not activate"
    );
    assert_eq!(store.active().unwrap().unwrap(), new);
}

#[test]
fn an_incomplete_staged_bundle_does_not_publish_any_of_its_files() {
    let root = tempfile::tempdir().unwrap();
    let store = AssetStore::open(root.path().join("assets")).unwrap();
    let first = manifest(
        "1",
        11,
        "c6ce303f2afd029639c46178cc233a099f531a566a8583e12699f259dc666fd5",
    );
    let old = store
        .install(&first, &source(root.path(), "old", b"first-model"))
        .unwrap();
    let mut update = first.clone();
    update.version = "2".into();
    let mut missing = update.files[0].clone();
    missing.path = "missing.bin".into();
    update.files.push(missing);
    assert!(store.install(&update, &root.path().join("old")).is_err());
    assert_eq!(store.active().unwrap().unwrap(), old);
    assert_eq!(
        fs::read_dir(root.path().join("assets/versions"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn an_invalid_activation_record_is_preserved_instead_of_overwritten() {
    let root = tempfile::tempdir().unwrap();
    let store = AssetStore::open(root.path().join("assets")).unwrap();
    let record = root.path().join("assets/active.json");
    let original = br#"{"schema_version":1,"current":"../../vocabulary.json","previous":null}"#;
    fs::write(&record, original).unwrap();
    let model = manifest(
        "1",
        11,
        "c6ce303f2afd029639c46178cc233a099f531a566a8583e12699f259dc666fd5",
    );
    let result = store.install(&model, &source(root.path(), "source", b"first-model"));
    assert!(result.is_err(), "invalid previous state was overwritten");
    assert_eq!(fs::read(record).unwrap(), original);
}

#[test]
fn reinstall_reuses_verified_files_and_repairs_changed_files_in_a_new_version() {
    let root = tempfile::tempdir().unwrap();
    let store = AssetStore::open(root.path().join("assets")).unwrap();
    let model = manifest(
        "1",
        11,
        "c6ce303f2afd029639c46178cc233a099f531a566a8583e12699f259dc666fd5",
    );
    let source = source(root.path(), "source", b"first-model");
    let installed = store.install(&model, &source).unwrap();
    assert_eq!(
        store
            .install(&model, &root.path().join("source-not-needed"))
            .unwrap(),
        installed
    );
    fs::write(installed.join("model.onnx"), b"wrong-model").unwrap();
    assert!(store.active().is_err());
    let repaired = store.install(&model, &source).unwrap();
    assert_ne!(repaired, installed);
    assert_eq!(
        fs::read(repaired.join("model.onnx")).unwrap(),
        b"first-model"
    );
    assert!(store.rollback().is_err());
    assert_eq!(store.active().unwrap().unwrap(), repaired);
}
