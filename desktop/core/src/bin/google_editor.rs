//! Narrow external editor invoked by the official CLI's Ctrl+G action. It
//! copies an owned temporary prompt into an exclusive evidence file and exits
//! without changing the prompt or sending terminal input.
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path},
};

const LIMIT: u64 = 64 * 1024;

fn main() {
    if let Err(error) = run() {
        // Never echo paths or draft contents to CLI terminal/logs.
        eprintln!("VoiceType draft capture failed ({error})");
        std::process::exit(2);
    }
}

fn run() -> io::Result<()> {
    let mut args = env::args_os().skip(1);
    let source = args.next().ok_or(io::ErrorKind::InvalidInput)?;
    if args.next().is_some() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let source = std::path::PathBuf::from(source);
    let temporary = std::path::PathBuf::from(
        env::var_os("VOICETYPE_GOOGLE_TMP").ok_or(io::ErrorKind::InvalidInput)?,
    );
    let capture = std::path::PathBuf::from(
        env::var_os("VOICETYPE_GOOGLE_CAPTURE").ok_or(io::ErrorKind::InvalidInput)?,
    );
    if !source.is_absolute()
        || !temporary.is_absolute()
        || !capture.is_absolute()
        || source.components().any(|c| c == Component::ParentDir)
        || !source.starts_with(&temporary)
        || source == temporary
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    private_dir(&temporary)
        .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "tmp directory"))?;
    private_dir(&capture)
        .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "capture directory"))?;
    let stage = match fs::read_to_string(capture.join("active-stage"))?.as_str() {
        "1" => 1,
        "2" => 2,
        "3" => 3,
        _ => return Err(io::ErrorKind::InvalidData.into()),
    };
    let parent = source.parent().ok_or(io::ErrorKind::InvalidInput)?;
    private_dir(parent)
        .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "source directory"))?;
    let before = fs::symlink_metadata(&source)?;
    if !before.file_type().is_file() || before.len() > LIMIT {
        return Err(io::ErrorKind::InvalidData.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.nlink() != 1
            || before.uid() != unsafe { libc::geteuid() }
            || before.mode() & 0o022 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "source metadata",
            ));
        }
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(&source)?;
    let opened = file.metadata()?;
    if !opened.is_file() || opened.len() > LIMIT {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    let after = fs::metadata(&source)?;
    if bytes.len() as u64 > LIMIT
        || opened.len() != bytes.len() as u64
        || opened.modified()? != after.modified()?
        || opened.len() != after.len()
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| io::ErrorKind::InvalidData)?;
    if text.contains('\0') {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let record = json!({"text":text,"bytes":bytes.len(),"sha256":format!("{:x}", Sha256::digest(&bytes)),
        "source_modified":false,"agent_prompt_submitted":false,"stage":stage});
    let data = serde_json::to_vec(&record)?;
    let name = format!("draft-{stage}.json");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(capture.join(name)) {
        Ok(mut out) => {
            out.write_all(&data)?;
            out.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

fn private_dir(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
    }
    Ok(())
}
