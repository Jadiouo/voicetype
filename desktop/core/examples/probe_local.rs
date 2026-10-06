//! Read-only diagnostic. Never sends start/stop or reads personal settings.
#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket = std::env::args_os()
        .nth(1)
        .ok_or("usage: probe_local SOCKET")?;
    let profile = tempfile::tempdir()?;
    let mut app = voicetype_app_core::Application::open(profile.path())?;
    let view = app.refresh_local_provider(
        std::path::Path::new(&socket),
        std::time::Duration::from_millis(300),
    );
    println!("{}", serde_json::to_string(&view.providers[0])?);
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("This diagnostic is for the Linux Unix-socket engine.");
    std::process::exit(1);
}
