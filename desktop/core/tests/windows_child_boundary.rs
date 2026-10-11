//! Public B seam: child protocol to coordinator and recovery, with the real
//! stdio owner. The fake child substitutes only the microphone/model process.
#![cfg(unix)]
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    time::{Duration, Instant},
};
use voicetype_app_core::{
    windows_engine::{EnginePaths, WindowsEngine},
    Application, DeliveryOutcome, DeliveryPort, ProviderEvent, TargetLease,
};
use voicetype_text::spelling::SpellingPaths;

struct Focus {
    now: &'static str,
}
impl DeliveryPort for Focus {
    fn commit_if_focused(&mut self, target: &TargetLease, _text: &str) -> DeliveryOutcome {
        if self.now == target.context_id {
            DeliveryOutcome::NotSent
        } else {
            DeliveryOutcome::FocusChanged
        }
    }
}

fn event(engine: &mut WindowsEngine) -> (voicetype_app_core::SessionKey, ProviderEvent) {
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(event) = engine.poll_event().unwrap() {
            return event;
        }
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
        executable: script,
        model_dir: model,
        vad_model: vad,
        profile: dir.path().join("profile"),
        vocabulary: dir.path().join("vocab.toml"),
        review_config: dir.path().join("review.json"),
        review_root: dir.path().join("review"),
        spelling: None,
    })
    .unwrap();
    let mut app = Application::open(dir.path()).unwrap();
    let key = app
        .start_dictation(TargetLease::new("win:100/edit:1", 1), &mut engine)
        .unwrap();
    let (got, recording) = event(&mut engine);
    assert_eq!(got, key);
    app.provider_event(
        got,
        recording,
        &mut Focus {
            now: "win:100/edit:2",
        },
    );
    app.stop_dictation(&mut engine).unwrap();
    let (got, result) = event(&mut engine);
    app.provider_event(
        got,
        result,
        &mut Focus {
            now: "win:100/edit:2",
        },
    );
    let (got, release) = event(&mut engine);
    app.provider_event(
        got,
        release,
        &mut Focus {
            now: "win:100/edit:2",
        },
    );
    let recovery = app
        .retained_text()
        .expect("other edit must not receive text");
    assert_eq!(recovery.text, "你好 GitHub");
    assert_eq!(recovery.reason, DeliveryOutcome::FocusChanged);
    assert!(!app.snapshot().dictation.busy);
}

fn spelling_paths(dir: &std::path::Path) -> SpellingPaths {
    SpellingPaths {
        executable: dir.join("spelling.exe"),
        model: dir.join("model.onnx"),
        tokenizer: dir.join("tokenizer.json"),
        model_sha256: "a".repeat(64),
        tokenizer_sha256: "b".repeat(64),
        threads: 1,
    }
}

#[test]
fn spelling_child_lost_after_ready_updates_status_without_losing_asr() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("fake-engine.py");
    fs::write(&script, concat!(
        "#!/usr/bin/env python3\n",
        "import json, sys\n",
        "for line in sys.stdin:\n",
        " m = json.loads(line)\n",
        " if m['type'] == 'desktop_status':\n",
        "  status = 'ready' if m['request'] == 1 else 'unavailable'\n",
        "  print(json.dumps({'type':'info','value':{'desktop_protocol':1,'request':m['request'],'session_busy':False,'spelling_status':status,'capabilities':['session_events','suspend']}}),flush=True)\n",
        " elif m['type'] == 'start':\n",
        "  print(json.dumps({'type':'state','session':m['session'],'value':'recording'}),flush=True)\n",
        " elif m['type'] == 'stop':\n",
        "  print(json.dumps({'type':'result','session':m['session'],'text':'今天 GitHub 很好'}),flush=True)\n",
        "  print(json.dumps({'type':'state','session':m['session'],'value':'idle'}),flush=True)\n",
    )).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let model = dir.path().join("model");
    fs::create_dir(&model).unwrap();
    let vad = dir.path().join("vad.onnx");
    fs::write(&vad, b"fixture").unwrap();
    let mut engine = WindowsEngine::start(EnginePaths {
        executable: script,
        model_dir: model,
        vad_model: vad,
        profile: dir.path().join("profile"),
        vocabulary: dir.path().join("vocab.toml"),
        review_config: dir.path().join("review.json"),
        review_root: dir.path().join("review"),
        spelling: Some(spelling_paths(dir.path())),
    })
    .unwrap();
    assert!(engine.spelling_ready());
    let until = Instant::now() + Duration::from_secs(2);
    while engine.spelling_ready() && Instant::now() < until {
        assert!(engine.poll_event().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !engine.spelling_ready(),
        "owned spelling loss must clear Ready"
    );
    let mut app = Application::open(dir.path()).unwrap();
    let key = app
        .start_dictation(TargetLease::new("win:edit:1", 1), &mut engine)
        .unwrap();
    let (got, recording) = event(&mut engine);
    assert_eq!(got, key);
    app.provider_event(got, recording, &mut Focus { now: "win:edit:1" });
    app.stop_dictation(&mut engine).unwrap();
    let (got, result) = event(&mut engine);
    app.provider_event(got, result, &mut Focus { now: "win:edit:1" });
    let (got, released) = event(&mut engine);
    app.provider_event(got, released, &mut Focus { now: "win:edit:1" });
    assert_eq!(app.retained_text().unwrap().text, "今天 GitHub 很好");
}

#[test]
fn a_healthy_status_reply_after_100ms_does_not_mark_spelling_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("slow-status.py");
    fs::write(&script, concat!(
        "#!/usr/bin/env python3\n",
        "import json, os, pathlib, sys, time\n",
        "for line in sys.stdin:\n",
        " m = json.loads(line)\n",
        " if m['type'] == 'desktop_status':\n",
        "  if m['request'] != 1:\n",
        "   pathlib.Path(os.environ['USERPROFILE'], 'status-observed').write_text('queried')\n",
        "   time.sleep(0.1)\n",
        "  print(json.dumps({'type':'info','value':{'desktop_protocol':1,'request':m['request'],'session_busy':False,'spelling_status':'ready','capabilities':['session_events','suspend']}}),flush=True)\n",
    )).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let model = dir.path().join("model");
    fs::create_dir(&model).unwrap();
    let vad = dir.path().join("vad.onnx");
    fs::write(&vad, b"fixture").unwrap();
    let mut engine = WindowsEngine::start(EnginePaths {
        executable: script,
        model_dir: model,
        vad_model: vad,
        profile: dir.path().join("profile"),
        vocabulary: dir.path().join("vocab.toml"),
        review_config: dir.path().join("review.json"),
        review_root: dir.path().join("review"),
        spelling: Some(spelling_paths(dir.path())),
    })
    .unwrap();
    let until = Instant::now() + Duration::from_millis(300);
    while Instant::now() < until {
        assert!(engine.poll_event().unwrap().is_none());
        assert!(
            engine.spelling_ready(),
            "100 ms status response must not be treated as a dead worker"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        dir.path().join("profile/status-observed").is_file(),
        "query was actually issued"
    );
}
