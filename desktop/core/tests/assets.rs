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

#[test]
fn cancelling_a_copy_keeps_the_previous_version_and_removes_partial_files() {
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
    // Cancellation must be observed during a file, not just between files.
    let bytes = vec![0u8; 1024 * 1024];
    let second = manifest(
        "2",
        bytes.len() as u64,
        "30e14955ebf1352266dc2ff8067e68104607e750abb9d3b36582b8af909fcb58",
    );
    let mut last_progress = 0;
    let result = store.install_with_progress(
        &second,
        &source(root.path(), "new", &bytes),
        |done, total| {
            assert_eq!(total, 1024 * 1024);
            last_progress = done;
            done == 0
        },
    );
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
    assert!(last_progress > 0 && last_progress < bytes.len() as u64);
    assert_eq!(store.active().unwrap().unwrap(), old);
    assert_eq!(
        fs::read_dir(root.path().join("assets/versions"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn a_pinned_download_installs_only_reviewed_files_from_the_archive() {
    use std::io::Cursor;
    use voicetype_app_core::setup::{install_download, DownloadSpec};
    let root = tempfile::tempdir().unwrap();
    let store = AssetStore::open(root.path().join("assets")).unwrap();
    let model = manifest(
        "1",
        11,
        "c6ce303f2afd029639c46178cc233a099f531a566a8583e12699f259dc666fd5",
    );
    let mut tar = tar::Builder::new(Vec::new());
    for (path, contents) in [
        ("bundle/model.onnx", &b"first-model"[..]),
        ("bundle/README.md", &b"not a runtime asset"[..]),
    ] {
        let mut header = tar::Header::new_ustar();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, path, contents).unwrap();
    }
    let mut input = bzip2::read::BzEncoder::new(
        Cursor::new(tar.into_inner().unwrap()),
        bzip2::Compression::fast(),
    );
    let mut download = Vec::new();
    std::io::Read::read_to_end(&mut input, &mut download).unwrap();
    // Network fixture provides this independently of the installer.
    use sha2::{Digest, Sha256};
    let spec: DownloadSpec = serde_json::from_value(serde_json::json!({
        "format":"tar.bz2", "strip_prefix":"bundle", "bytes":download.len(),
        "sha256":format!("{:x}", Sha256::digest(&download))
    }))
    .unwrap();
    let file = root.path().join("download");
    fs::write(&file, download).unwrap();
    let installed = install_download(&store, &model, &spec, &file, |_, _, _| true).unwrap();
    assert_eq!(
        fs::read(installed.join("model.onnx")).unwrap(),
        b"first-model"
    );
    assert!(!installed.join("README.md").exists());
    assert_eq!(store.active().unwrap().unwrap(), installed);

    // A changed body must fail before extracting or changing the active version.
    fs::write(&file, vec![0; spec.bytes as usize]).unwrap();
    assert!(install_download(&store, &model, &spec, &file, |_, _, _| true).is_err());
    assert_eq!(store.active().unwrap().unwrap(), installed);
}

fn archived_fixture(
    root: &Path,
    entries: &[(&str, tar::EntryType)],
) -> (voicetype_app_core::setup::DownloadSpec, std::path::PathBuf) {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut builder = tar::Builder::new(Vec::new());
    for (name, kind) in entries {
        let mut header = tar::Header::new_ustar();
        header.set_size(11);
        header.set_mode(0o644);
        header.set_entry_type(*kind);
        // Raw network fixture deliberately permits unsafe header paths which
        // tar::Builder::append_data would refuse to produce.
        header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
        header.set_cksum();
        builder.append(&header, &b"first-model"[..]).unwrap();
    }
    let mut input = bzip2::read::BzEncoder::new(
        std::io::Cursor::new(builder.into_inner().unwrap()),
        bzip2::Compression::fast(),
    );
    let mut bytes = Vec::new();
    input.read_to_end(&mut bytes).unwrap();
    let spec = serde_json::from_value(
        serde_json::json!({"format":"tar.bz2","strip_prefix":"bundle",
        "bytes":bytes.len(),"sha256":format!("{:x}", Sha256::digest(&bytes))}),
    )
    .unwrap();
    let path = root.join("archive");
    fs::write(&path, bytes).unwrap();
    (spec, path)
}

#[test]
fn cancellation_during_extraction_preserves_the_previous_model() {
    use voicetype_app_core::setup::{install_download, SetupPhase};
    let root = tempfile::tempdir().unwrap();
    let store = AssetStore::open(root.path().join("assets")).unwrap();
    let mut model = manifest(
        "1",
        11,
        "c6ce303f2afd029639c46178cc233a099f531a566a8583e12699f259dc666fd5",
    );
    let old = store
        .install(&model, &source(root.path(), "old", b"first-model"))
        .unwrap();
    model.version = "2".into();
    let (spec, archive) = archived_fixture(
        root.path(),
        &[("bundle/model.onnx", tar::EntryType::Regular)],
    );
    let mut cancelled = false;
    let result = install_download(&store, &model, &spec, &archive, |phase, done, _| {
        cancelled |= phase == SetupPhase::Extracting && done > 0;
        !cancelled
    });
    assert!(cancelled && result.is_err());
    assert_eq!(store.active().unwrap().unwrap(), old);
    assert!(!fs::read_dir(root.path()).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("unpack-")));
}

#[test]
fn unsafe_archive_entries_never_publish_a_partially_extracted_model() {
    use voicetype_app_core::setup::install_download;
    let root = tempfile::tempdir().unwrap();
    let store = AssetStore::open(root.path().join("assets")).unwrap();
    let mut model = manifest(
        "1",
        11,
        "c6ce303f2afd029639c46178cc233a099f531a566a8583e12699f259dc666fd5",
    );
    let old = store
        .install(&model, &source(root.path(), "old", b"first-model"))
        .unwrap();
    model.version = "2".into();
    for unsafe_entry in [
        ("bundle/link", tar::EntryType::Symlink),
        ("bundle/../escape", tar::EntryType::Regular),
        ("bundle/model.onnx", tar::EntryType::Regular),
        ("outside/model.onnx", tar::EntryType::Regular),
        ("bundle/metadata", tar::EntryType::GNULongName),
    ] {
        let (spec, archive) = archived_fixture(
            root.path(),
            &[("bundle/model.onnx", tar::EntryType::Regular), unsafe_entry],
        );
        assert!(install_download(&store, &model, &spec, &archive, |_, _, _| true).is_err());
        assert_eq!(store.active().unwrap().unwrap(), old);
    }
}
