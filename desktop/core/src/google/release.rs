//! Exact official CLI images reviewed for the interactive voice protocol.
//! A present PATH entry or a version string alone never grants readiness.
use super::{GoogleInstallState, GoogleSetup};
use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct GoogleInstallInfo {
    pub state: GoogleInstallState,
    pub version: Option<&'static str>,
}

/// The official installer currently places Linux `agy` at ~/.local/bin/agy.
/// This is a read-only exact-image check, never a login or voice-ready claim.
pub fn inspect_linux_install(path: &Path) -> GoogleInstallInfo {
    let digest = match GoogleSetup::image_digest(path) {
        Ok(digest) => digest,
        Err(error) => {
            return GoogleInstallInfo {
                state: if error.kind() == io::ErrorKind::NotFound {
                    GoogleInstallState::Missing
                } else {
                    GoogleInstallState::Incompatible
                },
                version: None,
            }
        }
    };
    for release in [GoogleCliRelease::Linux1212, GoogleCliRelease::Linux131] {
        if release.supported_on_current_os() && digest == release.reviewed_sha256() {
            return GoogleInstallInfo {
                state: GoogleInstallState::OfficialCheckRequired,
                version: Some(release.version()),
            };
        }
    }
    GoogleInstallInfo {
        state: GoogleInstallState::Incompatible,
        version: None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoogleCliRelease {
    Linux1212,
    Linux131,
    Windows131,
}

impl GoogleCliRelease {
    pub fn version(self) -> &'static str {
        match self {
            Self::Linux1212 => "1.2.12",
            Self::Linux131 | Self::Windows131 => "1.3.1",
        }
    }

    pub fn reviewed_sha256(self) -> &'static str {
        match self {
            Self::Linux1212 => "ce6fdd9e7621ee9ac6eedaa337731ca1f235e412ff57cf9eabcd2aa23b3576ca",
            Self::Linux131 => "ce1bdaed3201bb84f35d69d2773caec4f18af52af00e8c25f6cace07e4359615",
            Self::Windows131 => "38f30c7dd1ed808f5cf98fe2014de3d30903035a4f0df02d3eb72a9ff8993741",
        }
    }

    pub fn supported_on_current_os(self) -> bool {
        matches!(
            (self, std::env::consts::OS),
            (Self::Linux1212 | Self::Linux131, "linux") | (Self::Windows131, "windows")
        )
    }

    pub fn setup(self, path: PathBuf) -> io::Result<GoogleSetup> {
        if !self.supported_on_current_os() {
            return Err(io::ErrorKind::Unsupported.into());
        }
        Ok(GoogleSetup::new(path, self.reviewed_sha256().into()))
    }
}
