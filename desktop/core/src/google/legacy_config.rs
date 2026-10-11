//! Read-only conflict gate for the separately installed daily Google addon.
//! A later explicit migration owns config backup/reload/rollback; this probe
//! never edits its settings or service.
use std::{fs, io, path::Path};

pub fn ctrl_caps_conflict(path: &Path) -> io::Result<bool> {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if data.len() > 16 * 1024 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut section = "";
    let mut enabled = false;
    let mut hold = false;
    let mut toggle = false;
    for line in data.lines().map(str::trim) {
        if line.starts_with('[') && line.ends_with(']') {
            section = &line[1..line.len() - 1];
        } else if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim();
            if section.is_empty() {
                if key == "Enabled" {
                    enabled = value.eq_ignore_ascii_case("true");
                }
                if key == "HoldToTalkEnabled" {
                    hold = value.eq_ignore_ascii_case("true");
                }
            } else if section == if hold { "HoldToTalkKey" } else { "ToggleKey" }
                && value.eq_ignore_ascii_case("Control+Caps_Lock")
            {
                toggle = true;
            }
        }
    }
    Ok(enabled && toggle)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_enabled_matching_shortcut_conflicts() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("googlevoiceinput.conf");
        assert!(!ctrl_caps_conflict(&path).unwrap());
        fs::write(
            &path,
            "Enabled=True\nHoldToTalkEnabled=False\n[ToggleKey]\n0=Control+Caps_Lock\n",
        )
        .unwrap();
        assert!(ctrl_caps_conflict(&path).unwrap());
        fs::write(
            &path,
            "Enabled=False\nHoldToTalkEnabled=False\n[ToggleKey]\n0=Control+Caps_Lock\n",
        )
        .unwrap();
        assert!(!ctrl_caps_conflict(&path).unwrap());
        fs::write(
            &path,
            "Enabled=True\nHoldToTalkEnabled=False\n[ToggleKey]\n0=Control+Shift+F9\n",
        )
        .unwrap();
        assert!(!ctrl_caps_conflict(&path).unwrap());
    }
}
