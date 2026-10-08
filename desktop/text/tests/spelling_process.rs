use std::{process::Command, time::Duration};
use voicetype_text::spelling::OwnedSpelling;

fn fixture(source: &str) -> Command {
    let mut command = Command::new(if cfg!(windows) { "python" } else { "python3" });
    command.args(["-u", "-c", source]);
    command
}

#[test]
fn owned_cpu_worker_corrects_only_valid_edits_and_is_reaped_on_shutdown() {
    let mut worker = OwnedSpelling::start(
        fixture(
            r#"
import json, sys
print(json.dumps(dict(v=1, status='ready', provider='CPUExecutionProvider')), flush=True)
for line in sys.stdin.buffer:
    r=json.loads(line)
    edits=[] if '新情' in r['terms'] else [dict(start=2, source='新', target='心')]
    print(json.dumps(dict(v=1,id=r['id'],status='applied' if edits else 'unchanged',
                         model_calls=1,edits=edits)),flush=True)
"#,
        ),
        Duration::from_secs(3),
    )
    .unwrap();
    assert!(worker.is_running().unwrap());
    let input = "今天新情很好，GitHub terminal。";
    assert_eq!(
        worker.correct(input, &[]),
        "今天心情很好，GitHub terminal。"
    );
    assert_eq!(worker.correct(input, &["新情".into()]), input);
    worker.shutdown().unwrap();
    assert!(!worker.is_running().unwrap());
    assert_eq!(worker.process_id(), None);
    assert_eq!(worker.correct(input, &[]), input);
}

#[test]
fn a_valid_late_response_preserves_text_and_does_not_contaminate_the_next_request() {
    let mut worker = OwnedSpelling::start(
        fixture(
            r#"
import json, sys, time
print(json.dumps(dict(v=1, status='ready', provider='CPUExecutionProvider')), flush=True)
for line in sys.stdin.buffer:
    r=json.loads(line)
    if r['text'].startswith('慢'): time.sleep(.3)
    print(json.dumps(dict(v=1,id=r['id'],status='applied',model_calls=1,
                         edits=[dict(start=2,source='新',target='心')])),flush=True)
"#,
        ),
        Duration::from_secs(3),
    )
    .unwrap();
    let slow = "慢的新情 terminal。";
    let started = std::time::Instant::now();
    assert_eq!(worker.correct(slow, &[]), slow);
    assert!(started.elapsed() < Duration::from_millis(230));
    assert_eq!(worker.correct("今天新情很好。", &[]), "今天新情很好。");
    std::thread::sleep(Duration::from_millis(350));
    assert!(
        worker.healthy(),
        "a valid late frame is not a permanent pipe fault"
    );
    assert_eq!(worker.correct("今天新情很好。", &[]), "今天心情很好。");
    // Even a syntactically valid worker response cannot modify a protected name.
    assert_eq!(
        worker.correct("今天新情很好。", &["新情".into()]),
        "今天新情很好。"
    );
    worker.shutdown().unwrap();
}

#[test]
fn dead_child_or_invalid_frame_marks_the_owned_worker_unavailable() {
    for source in [
        "import json,sys,time; print(json.dumps(dict(v=1,status='ready',provider='CPUExecutionProvider')),flush=True); time.sleep(.25)",
        "import json,sys; print(json.dumps(dict(v=1,status='ready',provider='CPUExecutionProvider')),flush=True); sys.stdin.readline(); print('{bad json}',flush=True)",
    ] {
        let mut worker = OwnedSpelling::start(fixture(source), Duration::from_secs(3)).unwrap();
        let input = "今天新情很好。GitHub 2026";
        if source.contains("sleep") {
            std::thread::sleep(Duration::from_millis(400));
        }
        assert_eq!(worker.correct(input, &[]), input);
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while worker.healthy() {
            assert!(std::time::Instant::now() < until, "dead or corrupt worker remained ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(worker.correct(input, &[]), input);
        worker.shutdown().unwrap();
    }
}

#[test]
fn startup_requires_a_cpu_handshake_and_blocked_workers_can_be_reaped() {
    for source in [
        "import time; time.sleep(10)",
        "print('{\"v\":1,\"status\":\"ready\",\"provider\":\"CUDAExecutionProvider\"}',flush=True)",
        "print('x'*32769,flush=True)",
    ] {
        let started = std::time::Instant::now();
        assert!(OwnedSpelling::start(fixture(source), Duration::from_millis(120)).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    let mut worker = OwnedSpelling::start(
        fixture(
            r#"
import json, time
print(json.dumps(dict(v=1,status='ready',provider='CPUExecutionProvider')),flush=True)
time.sleep(10)
"#,
        ),
        Duration::from_secs(3),
    )
    .unwrap();
    assert_eq!(worker.correct("今天新情很好。", &[]), "今天新情很好。");
    let started = std::time::Instant::now();
    worker.shutdown().unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!worker.is_running().unwrap());
}
