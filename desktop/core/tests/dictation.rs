use voicetype_app_core::{
    AppError, Application, DeliveryOutcome, DeliveryPort, Provider, ProviderCommand, ProviderEvent,
    ProviderPort, SessionKey, TargetLease,
};

// Only the external provider and OS delivery boundaries are substituted. The
// application, settings persistence and session coordinator run unchanged.
#[derive(Default)]
struct EngineProcess {
    received: Vec<ProviderCommand>,
}
impl ProviderPort for EngineProcess {
    fn send(
        &mut self,
        command: ProviderCommand,
    ) -> Result<(), voicetype_app_core::CommandRejected> {
        self.received.push(command);
        Ok(())
    }
}

#[derive(Default)]
struct InputContext {
    inserted: Vec<(TargetLease, String)>,
    focused: Option<TargetLease>,
    attempts: usize,
}
impl DeliveryPort for InputContext {
    fn commit_if_focused(&mut self, target: &TargetLease, text: &str) -> DeliveryOutcome {
        self.attempts += 1;
        if self.focused.as_ref().is_some_and(|lease| lease != target) {
            return DeliveryOutcome::FocusChanged;
        }
        self.inserted.push((target.clone(), text.into()));
        DeliveryOutcome::Delivered
    }
}

#[test]
fn recording_keeps_its_provider_and_target_until_one_final_result_and_cleanup() {
    let profile = tempfile::tempdir().unwrap();
    let mut app = Application::open(profile.path()).unwrap();
    app.select_provider(Provider::Google).unwrap();
    let mut engine = EngineProcess::default();
    let mut input = InputContext::default();
    let target = TargetLease::new("editor-1", 42);
    let key: SessionKey = app.start_dictation(target.clone(), &mut engine).unwrap();
    assert_eq!(key.provider(), Provider::Google);
    assert!(matches!(
        app.select_provider(Provider::Local),
        Err(AppError::DictationBusy)
    ));
    app.provider_event(key, ProviderEvent::Recording, &mut input);
    app.stop_dictation(&mut engine).unwrap();
    app.stop_dictation(&mut engine).unwrap();
    let text = "請 review 這份 PR，保留 Google Antigravity。";
    app.provider_event(key, ProviderEvent::Final(text.into()), &mut input);
    app.provider_event(key, ProviderEvent::Final(text.into()), &mut input);
    assert_eq!(input.inserted, vec![(target.clone(), text.into())]);
    assert_eq!(
        engine.received,
        vec![
            ProviderCommand::Start {
                key,
                target,
                context: Default::default()
            },
            ProviderCommand::Stop { key },
        ]
    );
    assert!(matches!(
        app.select_provider(Provider::Local),
        Err(AppError::DictationBusy)
    ));
    app.provider_event(key, ProviderEvent::Released, &mut input);
    app.select_provider(Provider::Local).unwrap();
    assert_eq!(
        Application::open(profile.path())
            .unwrap()
            .snapshot()
            .selected_provider,
        Provider::Local
    );
}

#[test]
fn cancelling_invalidates_text_immediately_but_keeps_the_engine_reserved_until_release() {
    let profile = tempfile::tempdir().unwrap();
    let mut app = Application::open(profile.path()).unwrap();
    let mut engine = EngineProcess::default();
    let mut input = InputContext::default();
    let target = TargetLease::new("editor-1", 1);
    let old = app.start_dictation(target.clone(), &mut engine).unwrap();
    app.provider_event(old, ProviderEvent::Recording, &mut input);
    app.stop_dictation(&mut engine).unwrap();
    app.cancel_dictation(&mut engine).unwrap();
    app.cancel_dictation(&mut engine).unwrap();
    assert!(matches!(app.reload(), Err(AppError::DictationBusy)));
    assert!(matches!(
        app.start_dictation(target.clone(), &mut engine),
        Err(AppError::DictationBusy)
    ));
    app.provider_event(
        old,
        ProviderEvent::Final("cancelled text".into()),
        &mut input,
    );
    assert!(input.inserted.is_empty());
    assert_eq!(
        engine
            .received
            .iter()
            .filter(|c| matches!(c, ProviderCommand::Cancel { .. }))
            .count(),
        1
    );
    app.provider_event(old, ProviderEvent::Released, &mut input);
    app.select_provider(Provider::Google).unwrap();
    let current = app.start_dictation(target, &mut engine).unwrap();
    assert_ne!(current, old);
    app.provider_event(current, ProviderEvent::Recording, &mut input);
    app.stop_dictation(&mut engine).unwrap();
    app.provider_event(old, ProviderEvent::Released, &mut input);
    app.provider_event(old, ProviderEvent::Final("stale text".into()), &mut input);
    assert!(matches!(
        app.select_provider(Provider::Local),
        Err(AppError::DictationBusy)
    ));
    app.provider_event(
        current,
        ProviderEvent::Final("current text".into()),
        &mut input,
    );
    assert_eq!(input.inserted.len(), 1);
    assert_eq!(input.inserted[0].1, "current text");
}

#[test]
fn releasing_the_shortcut_before_capture_is_ready_cancels_instead_of_recording_later() {
    let profile = tempfile::tempdir().unwrap();
    let mut app = Application::open(profile.path()).unwrap();
    let mut engine = EngineProcess::default();
    let mut input = InputContext::default();
    let key = app
        .start_dictation(TargetLease::new("editor", 1), &mut engine)
        .unwrap();
    app.stop_dictation(&mut engine).unwrap();
    assert_eq!(
        engine.received.last(),
        Some(&ProviderCommand::Cancel { key })
    );
    app.provider_event(key, ProviderEvent::Recording, &mut input);
    app.stop_dictation(&mut engine).unwrap();
    app.provider_event(key, ProviderEvent::Final("too late".into()), &mut input);
    assert!(input.inserted.is_empty());
    assert_eq!(engine.received.len(), 2);
}

#[test]
fn changing_fields_in_the_same_window_retains_text_and_never_retries_automatically() {
    let profile = tempfile::tempdir().unwrap();
    let mut app = Application::open(profile.path()).unwrap();
    let mut engine = EngineProcess::default();
    let original = TargetLease::new("editor-window", 1);
    let mut input = InputContext {
        focused: Some(original.clone()),
        ..Default::default()
    };
    let key = app.start_dictation(original.clone(), &mut engine).unwrap();
    app.provider_event(key, ProviderEvent::Recording, &mut input);
    app.stop_dictation(&mut engine).unwrap();
    input.focused = Some(TargetLease::new("editor-window", 2));
    let text = "這段英文 GitHub 應該保留。";
    app.provider_event(key, ProviderEvent::Final(text.into()), &mut input);
    input.focused = Some(original);
    app.provider_event(key, ProviderEvent::Final(text.into()), &mut input);
    app.provider_event(key, ProviderEvent::Released, &mut input);
    assert!(input.inserted.is_empty());
    assert_eq!(input.attempts, 1);
    let retained = app.retained_text().unwrap();
    assert_eq!(retained.text, text);
    assert_eq!(retained.reason, DeliveryOutcome::FocusChanged);
    app.reload().unwrap();
    assert_eq!(
        app.retained_text().map(|result| result.text.as_str()),
        Some(text)
    );
}

#[test]
fn a_provider_timeout_is_visible_and_invalidates_a_late_result_before_cleanup() {
    let profile = tempfile::tempdir().unwrap();
    let mut app = Application::open(profile.path()).unwrap();
    let mut engine = EngineProcess::default();
    let mut input = InputContext::default();
    let key = app
        .start_dictation(TargetLease::new("editor", 1), &mut engine)
        .unwrap();
    app.provider_event(key, ProviderEvent::Recording, &mut input);
    app.stop_dictation(&mut engine).unwrap();
    app.provider_event(
        key,
        ProviderEvent::Failed(voicetype_app_core::SessionFailure::TimedOut),
        &mut input,
    );
    app.provider_event(key, ProviderEvent::Final("late result".into()), &mut input);
    assert!(input.inserted.is_empty());
    assert_eq!(
        app.snapshot().dictation.failure,
        Some(voicetype_app_core::SessionFailure::TimedOut)
    );
    assert!(app.snapshot().dictation.busy);
    app.provider_event(key, ProviderEvent::Released, &mut input);
    assert!(!app.snapshot().dictation.busy);
    assert_eq!(
        app.snapshot().dictation.failure,
        Some(voicetype_app_core::SessionFailure::TimedOut)
    );
}

#[test]
fn an_unavailable_local_provider_never_falls_back_to_google() {
    struct UnavailableProcess(Vec<ProviderCommand>);
    impl ProviderPort for UnavailableProcess {
        fn send(
            &mut self,
            command: ProviderCommand,
        ) -> Result<(), voicetype_app_core::CommandRejected> {
            self.0.push(command);
            Err(voicetype_app_core::CommandRejected)
        }
    }
    let profile = tempfile::tempdir().unwrap();
    let mut app = Application::open(profile.path()).unwrap();
    let mut engine = UnavailableProcess(vec![]);
    assert!(matches!(
        app.start_dictation(TargetLease::new("editor", 1), &mut engine),
        Err(AppError::ProviderUnavailable)
    ));
    assert_eq!(engine.0.len(), 1);
    assert!(
        matches!(&engine.0[0], ProviderCommand::Start { key, .. } if key.provider() == Provider::Local)
    );
    assert!(!app.snapshot().dictation.busy);
    assert_eq!(app.snapshot().selected_provider, Provider::Local);
}

#[test]
fn partial_os_insertion_is_retained_with_a_warning_and_not_retried() {
    struct PartialInput(usize);
    impl DeliveryPort for PartialInput {
        fn commit_if_focused(&mut self, _: &TargetLease, _: &str) -> DeliveryOutcome {
            self.0 += 1;
            DeliveryOutcome::Partial
        }
    }
    let profile = tempfile::tempdir().unwrap();
    let mut app = Application::open(profile.path()).unwrap();
    let mut engine = EngineProcess::default();
    let mut input = PartialInput(0);
    let key = app
        .start_dictation(TargetLease::new("editor", 1), &mut engine)
        .unwrap();
    app.provider_event(key, ProviderEvent::Recording, &mut input);
    app.stop_dictation(&mut engine).unwrap();
    app.provider_event(key, ProviderEvent::Final("完整文字".into()), &mut input);
    app.provider_event(key, ProviderEvent::Final("完整文字".into()), &mut input);
    assert_eq!(input.0, 1);
    let retained = app.retained_text().unwrap();
    assert_eq!(retained.text, "完整文字");
    assert_eq!(retained.reason, DeliveryOutcome::Partial);
}

#[test]
fn invalid_final_text_never_reaches_the_input_adapter() {
    for text in [
        " \t\n".to_string(),
        "a".repeat(65_537),
        "hello\0world".into(),
    ] {
        let profile = tempfile::tempdir().unwrap();
        let mut app = Application::open(profile.path()).unwrap();
        let mut engine = EngineProcess::default();
        let mut input = InputContext::default();
        let key = app
            .start_dictation(TargetLease::new("editor", 1), &mut engine)
            .unwrap();
        app.provider_event(key, ProviderEvent::Recording, &mut input);
        app.stop_dictation(&mut engine).unwrap();
        app.provider_event(key, ProviderEvent::Final(text), &mut input);
        assert_eq!(input.attempts, 0);
        assert_eq!(
            app.snapshot().dictation.failure,
            Some(voicetype_app_core::SessionFailure::InvalidResult)
        );
        app.provider_event(
            key,
            ProviderEvent::Final("repeated result".into()),
            &mut input,
        );
        assert_eq!(input.attempts, 0);
    }
}
