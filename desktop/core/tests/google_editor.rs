#![cfg(target_os = "linux")]

use std::{fs, process::Command};

#[test]
fn external_editor_captures_two_unchanged_drafts_without_submitting_them() {
    let root = tempfile::tempdir().unwrap();
    let temporary = root.path().join("tmp");
    let captures = root.path().join("capture");
    fs::create_dir(&temporary).unwrap();
    fs::create_dir(&captures).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&captures, fs::Permissions::from_mode(0o700)).unwrap();
    let draft = temporary.join("prompt.txt");
    fs::write(&draft, "請 push 到 GitHub。").unwrap();
    fs::set_permissions(&draft, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fs::read(&draft).unwrap();
    let editor = env!("CARGO_BIN_EXE_voicetype-google-editor");
    for (stage, expected) in ["draft-1.json", "draft-2.json", "draft-3.json"]
        .into_iter()
        .enumerate()
    {
        fs::write(captures.join("active-stage"), format!("{}", stage + 1)).unwrap();
        let status = Command::new(editor)
            .arg(&draft)
            .env("VOICETYPE_GOOGLE_TMP", &temporary)
            .env("VOICETYPE_GOOGLE_CAPTURE", &captures)
            .status()
            .unwrap();
        assert!(status.success());
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(captures.join(expected)).unwrap()).unwrap();
        assert_eq!(record["text"], "請 push 到 GitHub。");
        assert_eq!(record["stage"], stage + 1);
        if stage == 0 {
            // A Ctrl+G retry must not consume the next capture slot.
            let repeated = Command::new(editor)
                .arg(&draft)
                .env("VOICETYPE_GOOGLE_TMP", &temporary)
                .env("VOICETYPE_GOOGLE_CAPTURE", &captures)
                .status()
                .unwrap();
            assert!(repeated.success());
            assert!(!captures.join("draft-2.json").exists());
        }
    }
    assert_eq!(fs::read(draft).unwrap(), before);
}
