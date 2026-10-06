use anyhow::{bail, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    SenseVoice,
    Nano,
}

impl Profile {
    pub fn parse(value: Option<&str>) -> Result<Self> {
        match value {
            None | Some("sensevoice") => Ok(Self::SenseVoice),
            Some("nano") => Ok(Self::Nano),
            Some(other) => {
                bail!("unknown VOICETYPE_ASR_PROFILE {other:?}; choose sensevoice or nano")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_remains_sensevoice_and_unknown_profiles_never_fall_back() {
        assert_eq!(Profile::parse(None).unwrap(), Profile::SenseVoice);
        assert_eq!(
            Profile::parse(Some("sensevoice")).unwrap(),
            Profile::SenseVoice
        );
        assert_eq!(Profile::parse(Some("nano")).unwrap(), Profile::Nano);
        for value in ["", "auto", "Nano", "nan0", "whisper"] {
            assert!(Profile::parse(Some(value)).is_err());
        }
    }
}
