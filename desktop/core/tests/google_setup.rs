use sha2::{Digest, Sha256};
use std::fs;
use voicetype_app_core::google::{GoogleInstallState, GoogleSetup};

#[test]
fn setup_distinguishes_missing_and_unreviewed_cli_from_login_not_yet_verified() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("agy");
    let setup = GoogleSetup::new(path.clone(), "0".repeat(64));
    assert_eq!(setup.inspect(), GoogleInstallState::Missing);
    fs::write(&path, b"official-version-A").unwrap();
    assert_eq!(setup.inspect(), GoogleInstallState::Incompatible);
    let setup = GoogleSetup::new(path, format!("{:x}", Sha256::digest(b"official-version-A")));
    assert_eq!(setup.inspect(), GoogleInstallState::OfficialCheckRequired);
}
