#![cfg(unix)]

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    thread,
    time::Duration,
};
use voicetype_app_core::{Application, Availability};

#[test]
fn a_live_local_engine_is_discovered_without_starting_recording() {
    let profile = tempfile::tempdir().unwrap();
    let socket = profile.path().join("engine.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut command = String::new();
        BufReader::new(peer.try_clone().unwrap())
            .read_line(&mut command)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&command).unwrap(),
            serde_json::json!({"type":"ping"})
        );
        peer.write_all(b"{\"type\":\"pong\"}\n").unwrap();
    });
    let mut app = Application::open(profile.path()).unwrap();
    let state = app.refresh_local_provider(&socket, Duration::from_millis(300));
    assert_eq!(
        state.providers[0].availability,
        Availability::ServiceAvailable
    );
    server.join().unwrap();
}

#[test]
fn an_unresponsive_engine_reports_a_timeout_without_blocking_settings() {
    let profile = tempfile::tempdir().unwrap();
    let socket = profile.path().join("engine.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(peer.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        // Deliberately do not reply until the caller closes its connection.
        use std::io::Read;
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut byte = [0];
        let _ = peer.read(&mut byte);
    });
    let mut app = Application::open(profile.path()).unwrap();
    let started = std::time::Instant::now();
    let state = app.refresh_local_provider(&socket, Duration::from_millis(50));
    assert_eq!(state.providers[0].availability, Availability::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(1));
    // An unavailable local engine must not prevent choosing Google for setup.
    app.select_provider(voicetype_app_core::Provider::Google)
        .unwrap();
    server.join().unwrap();
}

#[test]
fn an_unexpected_engine_reply_is_not_reported_as_ready() {
    let profile = tempfile::tempdir().unwrap();
    let socket = profile.path().join("engine.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(peer.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        peer.write_all(b"{\"type\":\"result\",\"session\":1,\"text\":\"unexpected\"}\n")
            .unwrap();
    });
    let mut app = Application::open(profile.path()).unwrap();
    let state = app.refresh_local_provider(&socket, Duration::from_millis(300));
    assert_eq!(state.providers[0].availability, Availability::Incompatible);
    server.join().unwrap();
}

#[test]
fn a_missing_engine_is_offline_and_an_oversized_reply_is_rejected() {
    let profile = tempfile::tempdir().unwrap();
    let socket = profile.path().join("engine.sock");
    let mut app = Application::open(profile.path()).unwrap();
    assert_eq!(
        app.refresh_local_provider(&socket, Duration::from_millis(300))
            .providers[0]
            .availability,
        Availability::Offline
    );
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(peer.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        let _ = peer.write_all(&vec![b' '; 8192]);
    });
    assert_eq!(
        app.refresh_local_provider(&socket, Duration::from_millis(300))
            .providers[0]
            .availability,
        Availability::Incompatible
    );
    server.join().unwrap();
    std::fs::remove_file(&socket).unwrap();
    assert_eq!(
        app.refresh_local_provider(&socket, Duration::from_millis(300))
            .providers[0]
            .availability,
        Availability::Offline
    );
}
