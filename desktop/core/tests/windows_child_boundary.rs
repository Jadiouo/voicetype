//! Public B seam: child protocol to coordinator and recovery, with the real
//! stdio owner. The fake child substitutes only the microphone/model process.
#![cfg(unix)]
use std::{fs, os::unix::fs::PermissionsExt, time::{Duration, Instant}};
use voicetype_app_core::{Application, DeliveryOutcome, DeliveryPort,
    ProviderEvent, TargetLease, windows_engine::{EnginePaths, WindowsEngine}};

struct Focus { now: &'static str }
impl DeliveryPort for Focus {
    fn commit_if_focused(&mut self, target: &TargetLease, _text: &str) -> DeliveryOutcome {
        if self.now == target.context_id { DeliveryOutcome::NotSent }
        else { DeliveryOutcome::FocusChanged }
    }
}

fn event(engine: &mut WindowsEngine) -> (voicetype_app_core::SessionKey, ProviderEvent) {
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(event) = engine.poll_event().unwrap() { return event; }
        assert!(Instant::now() < until, "fake child gave no event");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn same_window_other_editable_retains_child_result() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("fake-engine.py");
    fs::write(&script, concat!(
        "#!/usr/bin/env python3\n",
        "import json, sys\n",
        "for line in sys.stdin:\n",
        " m = json.loads(line)\n",
        " if m['type'] == 'desktop_status':\n",
        "  print(json.dumps({'type':'info','value':{'desktop_protocol':1,'request':1,'session_busy':False,'spelling_status':'disabled','capabilities':['session_events','suspend']}}),flush=True)\n",
        " elif m['type'] == 'start':\n",
        "  print(json.dumps({'type':'state','session':m['session'],'value':'recording'}),flush=True)\n",
        " elif m['type'] == 'stop':\n",
        "  print(json.dumps({'type':'result','session':m['session'],'text':'你好 GitHub'}),flush=True)\n",
        "  print(json.dumps({'type':'state','session':m['session'],'value':'idle'}),flush=True)\n",
    )).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let model = dir.path().join("model");
    fs::create_dir(&model).unwrap();
    let vad = dir.path().join("vad.onnx");
    fs::write(&vad, b"fixture").unwrap();
    let mut engine = WindowsEngine::start(EnginePaths {
        executable: script, model_dir: model, vad_model: vad,
        profile: dir.path().join("profile"), vocabulary: dir.path().join("vocab.toml"),
        review_config: dir.path().join("review.json"), review_root: dir.path().join("review"),
        spelling: None,
    }).unwrap();
    let mut app = Application::open(dir.path()).unwrap();
    let key = app.start_dictation(TargetLease::new("win:100/edit:1", 1), &mut engine).unwrap();
    let (got, recording) = event(&mut engine);
    assert_eq!(got, key);
    app.provider_event(got, recording, &mut Focus { now: "win:100/edit:2" });
    app.stop_dictation(&mut engine).unwrap();
    let (got, result) = event(&mut engine);
    app.provider_event(got, result, &mut Focus { now: "win:100/edit:2" });
    let (got, release) = event(&mut engine);
    app.provider_event(got, release, &mut Focus { now: "win:100/edit:2" });
    let recovery = app.retained_text().expect("other edit must not receive text");
    assert_eq!(recovery.text, "你好 GitHub");
    assert_eq!(recovery.reason, DeliveryOutcome::FocusChanged);
    assert!(!app.snapshot().dictation.busy);
}
