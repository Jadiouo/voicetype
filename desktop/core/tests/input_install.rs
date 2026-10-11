#![cfg(target_os = "linux")]
use std::fs;
use voicetype_app_core::{assets::AssetManifest, input_install::FcitxInstaller};

#[test]
fn explicit_module_install_and_restore_preserve_the_previous_registration_and_user_files() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("profile");
    let registration = root.path().join("data/fcitx5/addon/voicetype.conf");
    fs::create_dir_all(registration.parent().unwrap()).unwrap();
    let previous = b"[Addon]\nLibrary=/previous/libvoicetype\nEnabled=True\n";
    fs::write(&registration, previous).unwrap();
    fs::create_dir(&profile).unwrap();
    fs::write(profile.join("vocabulary.json"), b"personal words").unwrap();
    let manifest: AssetManifest = serde_json::from_str(r#"{
      "schema_version":1,"id":"fcitx-input","version":"fixture","platform":"linux-x86_64",
      "source_url":"https://example.org/module","license":"GPL-3.0-only",
      "files":[{"path":"libvoicetype.so","bytes":3,
      "sha256":"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad","executable":false}]
    }"#).unwrap();
    let installer = FcitxInstaller::new(profile.clone(), registration.clone(), manifest).unwrap();
    assert_eq!(fs::read(&registration).unwrap(), previous);
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("libvoicetype.so"), b"abc").unwrap();
    let installed = installer.install(&source).unwrap();
    assert_eq!(fs::read(installed.join("libvoicetype.so")).unwrap(), b"abc");
    let active = fs::read_to_string(&registration).unwrap();
    assert!(active.contains(&format!(
        "Library={}\n",
        installed.join("libvoicetype").display()
    )));
    assert!(profile.join("fcitx-rollback.json").is_file());
    installer.install(&source).unwrap(); // Repeated installs keep the original backup.
                                         // An external editor's later change is not ours to overwrite, on either path.
    fs::write(&registration, b"[Addon]\nLibrary=/external/change\n").unwrap();
    assert!(installer.restore().is_err());
    assert!(installer.install(&source).is_err());
    assert_eq!(
        fs::read(&registration).unwrap(),
        b"[Addon]\nLibrary=/external/change\n"
    );
    fs::write(&registration, &active).unwrap();
    fs::write(source.join("libvoicetype.so"), b"bad").unwrap();
    // Already verified immutable assets can be reused independently of source.
    installer.install(&source).unwrap();
    assert!(installer.restore().unwrap());
    assert_eq!(fs::read(&registration).unwrap(), previous);
    assert_eq!(
        fs::read(profile.join("vocabulary.json")).unwrap(),
        b"personal words"
    );
    assert!(!installer.restore().unwrap());
    // Clean-account installation restores absence, without deleting other files.
    fs::remove_file(&registration).unwrap();
    installer.install(&source).unwrap();
    assert!(installer.restore().unwrap());
    assert!(!registration.exists());
    // A symlink is preserved; never read or replace its target as a registration.
    std::os::unix::fs::symlink(profile.join("vocabulary.json"), &registration).unwrap();
    assert!(installer.install(&source).is_err());
    assert!(fs::symlink_metadata(&registration)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read(profile.join("vocabulary.json")).unwrap(),
        b"personal words"
    );
}
