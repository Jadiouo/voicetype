#![cfg(target_os = "linux")]

use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, ErrorKind, Write},
    os::unix::net::UnixListener,
    sync::mpsc,
    thread,
    time::Duration,
};
use voicetype_app_core::{local::LocalConnection, Application, TargetLease};

#[test]
fn an_oversized_fragmented_engine_frame_faults_without_releasing_the_session() {
    let profile = tempfile::tempdir().unwrap();
    let socket = profile.path().join("engine.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (first_sent, first_received) = mpsc::channel();
    let (finish, finish_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        peer.set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(peer.try_clone().unwrap());
        let mut command = String::new();
        reader.read_line(&mut command).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&command).unwrap(),
            json!({"type":"desktop_status","request":1})
        );
        writeln!(
            peer,
            "{}",
            json!({"type":"info","value":{
            "desktop_protocol":1,"request":1,"session_busy":false,
            "capabilities":["session_events","suspend"]}})
        )
        .unwrap();
        command.clear();
        reader.read_line(&mut command).unwrap();
        let command: Value = serde_json::from_str(&command).unwrap();
        assert_eq!(command["type"], "start");
        let frame = format!(
            "{}\n",
            json!({"type":"result","session":command["session"],
            "text":"x".repeat(256 * 1024)})
        );
        let split = 256 * 1024 - 1;
        peer.write_all(&frame.as_bytes()[..split]).unwrap();
        first_sent.send(()).unwrap();
        finish_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        peer.write_all(&frame.as_bytes()[split..]).unwrap();
    });
    let mut connection =
        LocalConnection::connect_to_process(&socket, std::process::id(), Duration::from_secs(1))
            .unwrap();
    let mut app = Application::open(profile.path()).unwrap();
    app.start_dictation(TargetLease::new("editor", 1), &mut connection)
        .unwrap();
    assert!(connection
        .poll_event(Duration::from_millis(100))
        .unwrap()
        .is_none());
    first_received.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(connection
        .poll_event(Duration::from_millis(10))
        .unwrap()
        .is_none());
    finish.send(()).unwrap();
    assert!(matches!(connection.poll_event(Duration::from_secs(1)),
        Err(error) if error.kind() == ErrorKind::InvalidData));
    assert!(app.snapshot().dictation.busy);
    assert!(matches!(connection.poll_event(Duration::from_millis(10)),
        Err(error) if error.kind() == ErrorKind::BrokenPipe));
    server.join().unwrap();
}
