use voicetype_app_core::{Application, Availability, Provider, ShortcutChoice};

#[test]
fn shortcut_request_is_per_provider_and_restart_keeps_last_confirmed_binding() {
    let home = tempfile::tempdir().unwrap();
    let mut app = Application::open(home.path()).unwrap();
    let initial = app.snapshot();
    assert_eq!(initial.shortcuts.local.requested, ShortcutChoice::Default);
    assert_eq!(initial.shortcuts.local.effective, ShortcutChoice::Default);
    assert_eq!(initial.shortcuts.google.requested, ShortcutChoice::Default);
    app.set_shortcut(Provider::Google, ShortcutChoice::CtrlShiftF9)
        .unwrap();
    let pending = app.snapshot();
    assert_eq!(
        pending.shortcuts.google.requested,
        ShortcutChoice::CtrlShiftF9
    );
    assert_eq!(pending.shortcuts.google.effective, ShortcutChoice::Default);
    assert_eq!(pending.shortcuts.local.requested, ShortcutChoice::Default);
    drop(app);
    let mut reopened = Application::open(home.path()).unwrap();
    assert_eq!(
        reopened.snapshot().shortcuts.google.requested,
        ShortcutChoice::CtrlShiftF9
    );
    assert_eq!(
        reopened.snapshot().shortcuts.google.effective,
        ShortcutChoice::Default
    );
    reopened
        .confirm_shortcut(Provider::Google, ShortcutChoice::CtrlShiftF9)
        .unwrap();
    assert_eq!(
        Application::open(home.path())
            .unwrap()
            .snapshot()
            .shortcuts
            .google
            .effective,
        ShortcutChoice::CtrlShiftF9
    );
}

#[test]
fn invalid_shortcut_preserves_original_settings_and_stale_writer_cannot_clobber_choice() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("desktop.json");
    let invalid = br#"{"schema_version":1,"selected_provider":"local","shortcuts":{"local":{"requested":"delete_everything"}}}"#;
    std::fs::write(&path, invalid).unwrap();
    assert!(Application::open(home.path()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), invalid);
    std::fs::remove_file(&path).unwrap();
    let mut stale = Application::open(home.path()).unwrap();
    let mut current = Application::open(home.path()).unwrap();
    current
        .set_shortcut(Provider::Local, ShortcutChoice::CtrlShiftF9)
        .unwrap();
    assert!(stale
        .set_shortcut(Provider::Google, ShortcutChoice::CtrlShiftF9)
        .is_err());
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(saved["shortcuts"]["local"]["requested"], "ctrl_shift_f9");
    assert_eq!(saved["shortcuts"]["google"]["requested"], "default");
}

#[test]
fn user_can_choose_google_and_keep_the_choice_after_reopening() {
    let home = tempfile::tempdir().unwrap();
    let mut app = Application::open(home.path()).unwrap();
    let initial = app.snapshot();
    assert_eq!(initial.selected_provider, Provider::Local);
    assert_eq!(initial.providers.len(), 2);
    assert_eq!(initial.providers[0].provider, Provider::Local);
    assert_eq!(initial.providers[1].provider, Provider::Google);
    assert!(initial
        .providers
        .iter()
        .all(|p| p.availability == Availability::NotConnected));
    app.select_provider(Provider::Google).unwrap();
    drop(app);
    let reopened = Application::open(home.path()).unwrap();
    assert_eq!(reopened.snapshot().selected_provider, Provider::Google);
    assert_eq!(reopened.snapshot().compute_device, "cpu");
}

#[test]
fn a_newer_app_configuration_is_preserved_and_requires_a_newer_app() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("desktop.json");
    let original = br#"{"schema_version":99,"selected_provider":"google"}"#;
    std::fs::write(&config, original).unwrap();
    assert!(Application::open(home.path()).is_err());
    assert_eq!(std::fs::read(config).unwrap(), original);
}

#[test]
fn an_older_open_window_cannot_overwrite_a_newer_choice() {
    let home = tempfile::tempdir().unwrap();
    let mut older = Application::open(home.path()).unwrap();
    let mut newer = Application::open(home.path()).unwrap();
    newer.select_provider(Provider::Google).unwrap();
    assert!(older.select_provider(Provider::Local).is_err());
    let reopened = Application::open(home.path()).unwrap();
    assert_eq!(reopened.snapshot().selected_provider, Provider::Google);
    assert_eq!(older.snapshot().selected_provider, Provider::Local);
}

#[test]
fn changing_provider_preserves_extension_settings_and_personal_files() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("desktop.json");
    std::fs::write(
        &config,
        r#"{"schema_version":1,"selected_provider":"local","future_option":{"enabled":true}}"#,
    )
    .unwrap();
    let vocab = home.path().join("vocab.toml");
    let vocabulary = b"# Personal words\nentry = []\n";
    std::fs::write(&vocab, vocabulary).unwrap();
    Application::open(home.path())
        .unwrap()
        .select_provider(Provider::Google)
        .unwrap();
    let data: serde_json::Value = serde_json::from_slice(&std::fs::read(config).unwrap()).unwrap();
    assert_eq!(data["future_option"]["enabled"], true);
    assert_eq!(std::fs::read(vocab).unwrap(), vocabulary);
}

#[test]
fn user_can_reload_after_a_conflict_and_then_save_their_next_choice() {
    let home = tempfile::tempdir().unwrap();
    let mut app = Application::open(home.path()).unwrap();
    Application::open(home.path())
        .unwrap()
        .select_provider(Provider::Google)
        .unwrap();
    assert!(app.select_provider(Provider::Local).is_err());
    assert_eq!(app.reload().unwrap().selected_provider, Provider::Google);
    app.select_provider(Provider::Local).unwrap();
    assert_eq!(
        Application::open(home.path())
            .unwrap()
            .snapshot()
            .selected_provider,
        Provider::Local
    );
}
#[test]
fn cpu_spelling_choice_persists_without_starting_a_provider() {
    let root = tempfile::tempdir().unwrap();
    let mut app = voicetype_app_core::Application::open(root.path()).unwrap();
    assert!(app.snapshot().spelling_enabled);
    assert!(!app.set_spelling_enabled(false).unwrap().spelling_enabled);
    let reopened = voicetype_app_core::Application::open(root.path()).unwrap();
    assert!(!reopened.snapshot().spelling_enabled);
    assert!(!reopened.snapshot().dictation.busy);
    assert_eq!(
        reopened.snapshot().selected_provider,
        voicetype_app_core::Provider::Local
    );
}

#[test]
fn spelling_choice_preserves_unknown_settings_and_rejects_a_stale_window() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("desktop.json");
    std::fs::write(
        &path,
        r#"{"schema_version":1,"selected_provider":"local","future_option":42}"#,
    )
    .unwrap();
    let mut stale = Application::open(root.path()).unwrap();
    let mut current = Application::open(root.path()).unwrap();
    current.set_spelling_enabled(false).unwrap();
    assert!(stale.set_spelling_enabled(true).is_err());
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(saved["future_option"], 42);
    assert_eq!(saved["spelling_enabled"], false);
}
