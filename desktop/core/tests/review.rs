use std::fs;
use voicetype_app_core::review::ReviewStore;

#[test]
fn sampling_requires_an_explicit_setting_and_preserves_existing_preferences() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config/review.json");
    let samples = home.path().join("data/review");
    let store = ReviewStore::open(config.clone(), samples.clone()).unwrap();
    let initial = store.settings().unwrap();
    assert!(!initial.enabled);
    assert_eq!(initial.daily_limit, 5);
    assert!(!config.exists());
    assert!(!samples.exists());
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(
        &config,
        r#"{"enabled":false,"daily_limit":3,"future":"kept"}"#,
    )
    .unwrap();
    assert!(store.set_enabled(&initial.revision, true).is_err());
    let loaded = store.settings().unwrap();
    let enabled = store.set_enabled(&loaded.revision, true).unwrap();
    assert!(enabled.enabled);
    assert_eq!(enabled.daily_limit, 3);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(config).unwrap()).unwrap()["future"],
        "kept"
    );
    assert!(
        !samples.exists(),
        "Enabling sampling must not create a fake recording"
    );
    assert!(!store.set_enabled(&enabled.revision, false).unwrap().enabled);
}

fn sample(root: &std::path::Path, id: &str, timestamp: u64) {
    let path = root.join(id);
    fs::create_dir_all(&path).unwrap();
    fs::write(
        path.join("record.json"),
        serde_json::to_vec(&serde_json::json!({
            "version":1,"id":id,"created_at":timestamp,"duration_ms":9000,"sample_rate":16000,
            "asr_text":"請 push 到 geeho。","output_text":"請 push 到 geeho。",
            "status":"pending","corrected_text":null,"reviewed_at":null,"future":"keep"
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn reviews_expire_after_seven_days_and_corrections_require_the_displayed_revision() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("review");
    let store = ReviewStore::open(home.path().join("review.json"), root.clone()).unwrap();
    let now = 1_800_000_000;
    assert!(store.list(now).unwrap().is_empty());
    assert!(!root.exists());
    sample(&root, "r-1-2-3", now - 60);
    sample(&root, "r-1-2-4", now - 7 * 86400);
    let items = store.list(now).unwrap();
    assert_eq!(items.len(), 1);
    assert!(!root.join("r-1-2-4").exists());
    let item = &items[0];
    assert_eq!(item.status, "pending");
    let corrected = store
        .review(&item.id, &item.revision, "請 push 到 GitHub。".into(), now)
        .unwrap();
    assert_eq!(corrected.status, "corrected");
    assert_eq!(
        corrected.corrected_text.as_deref(),
        Some("請 push 到 GitHub。")
    );
    assert!(store
        .review(&item.id, &item.revision, "stale window".into(), now)
        .is_err());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(root.join("r-1-2-3/record.json")).unwrap()
        )
        .unwrap()["future"],
        "keep"
    );
    assert!(store
        .review("../../outside", &item.revision, "unsafe".into(), now)
        .is_err());
    let confirmed = store
        .review(
            &corrected.id,
            &corrected.revision,
            item.output_text.clone(),
            now,
        )
        .unwrap();
    assert_eq!(confirmed.status, "correct");
    assert!(store.list(now + 7 * 86400).unwrap().is_empty());
}

#[test]
fn playback_and_delete_are_bound_to_the_selected_unexpired_record() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("review");
    let now = 1_800_000_000;
    sample(&root, "r-1-2-3", now);
    let store = ReviewStore::open(home.path().join("review.json"), root.clone()).unwrap();
    let item = store.list(now).unwrap().remove(0);
    let mut wav = b"RIFF".to_vec();
    wav.extend(288036u32.to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend([1, 0, 1, 0]);
    wav.extend(16000u32.to_le_bytes());
    wav.extend(32000u32.to_le_bytes());
    wav.extend([2, 0, 16, 0]);
    wav.extend(b"data");
    wav.extend(288000u32.to_le_bytes());
    wav.resize(288044, 0);
    fs::write(root.join("r-1-2-3/audio.wav"), &wav).unwrap();
    assert_eq!(store.audio(&item.id, &item.revision, now).unwrap(), wav);
    assert!(store
        .audio(&item.id, &item.revision, now + 7 * 86400)
        .is_err());
    wav[24] = 0;
    fs::write(root.join("r-1-2-3/audio.wav"), wav).unwrap();
    assert!(store.audio(&item.id, &item.revision, now).is_err());
    assert!(store.delete(&item.id, "stale", now).is_err());
    assert!(root.join(&item.id).exists());
    store.delete(&item.id, &item.revision, now).unwrap();
    assert!(!root.join(&item.id).exists());
}

#[test]
fn a_reviewed_rule_is_explicit_and_a_failed_promotion_can_be_retried() {
    use voicetype_app_core::vocabulary::Vocabulary;
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("review");
    let now = 1_800_000_000;
    sample(&root, "r-1-2-3", now);
    let store = ReviewStore::open(home.path().join("review.json"), root).unwrap();
    let vocab = home.path().join("vocab.toml");
    let item = store.list(now).unwrap().remove(0);
    let corrected = store
        .review(&item.id, &item.revision, "請 push 到 GitHub。".into(), now)
        .unwrap();
    assert!(
        !vocab.exists(),
        "Correcting a sentence is not permission to add a rule"
    );
    assert_eq!(corrected.suggested_rule.as_ref().unwrap().wrong, "geeho");
    fs::write(&vocab, "broken = [").unwrap();
    assert!(store
        .promote(&item.id, &corrected.revision, now, &vocab)
        .is_err());
    assert_eq!(fs::read_to_string(&vocab).unwrap(), "broken = [");
    let pending = store.list(now).unwrap().remove(0);
    assert!(pending.promotion_pending);
    assert!(store
        .review(&item.id, &pending.revision, "different edit".into(), now)
        .is_err());
    fs::remove_file(&vocab).unwrap();
    let saved = store
        .promote(&item.id, &pending.revision, now, &vocab)
        .unwrap();
    assert!(!saved.promotion_pending);
    assert_eq!(
        Vocabulary::open(vocab.clone())
            .unwrap()
            .preview("請 push 到 GEEHO。")
            .unwrap(),
        "請 push 到 GitHub。"
    );
    store
        .promote(&item.id, &saved.revision, now, &vocab)
        .unwrap();
    assert_eq!(Vocabulary::open(vocab).unwrap().snapshot().entries.len(), 1);
}

#[test]
fn suggested_rules_preserve_context_and_protect_confirmed_usage() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("review");
    let now = 1_800_000_000;
    let store = ReviewStore::open(home.path().join("review.json"), root.clone()).unwrap();
    let vocab = home.path().join("vocab.toml");
    let cases = [
        (
            "把修改 coming，然後 push。",
            "把修改 commit，然後 push。",
            Some("修改 coming"),
        ),
        ("coming soon", "commit soon", None),
        ("請 geeho，之後 coming。", "請 GitHub，之後 commit。", None),
        ("請用 `geeho`。", "請用 `GitHub`。", None),
        ("請 push 到 geeho。", "請 push 到 GitHub。", Some("geeho")),
    ];
    for (n, (before, after, expected)) in cases.iter().enumerate() {
        let id = format!("r-1-2-{n}");
        sample(&root, &id, now);
        let path = root.join(&id).join("record.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["output_text"] = serde_json::json!(before);
        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        let item = store
            .list(now)
            .unwrap()
            .into_iter()
            .find(|i| i.id == id)
            .unwrap();
        let saved = store
            .review(&id, &item.revision, (*after).into(), now)
            .unwrap();
        assert_eq!(
            saved.suggested_rule.as_ref().map(|p| p.wrong.as_str()),
            *expected,
            "{before}"
        );
    }
    sample(&root, "r-2-2-1", now);
    let other = store
        .list(now)
        .unwrap()
        .into_iter()
        .find(|i| i.id == "r-2-2-1")
        .unwrap();
    store
        .review(&other.id, &other.revision, other.output_text.clone(), now)
        .unwrap();
    let item = store
        .list(now)
        .unwrap()
        .into_iter()
        .find(|i| i.id == "r-1-2-4")
        .unwrap();
    assert!(store
        .promote(&item.id, &item.revision, now, &vocab)
        .is_err());
    assert!(
        !vocab.exists(),
        "A known correct usage must prevent a broad replacement"
    );
}

#[test]
fn damaged_and_abandoned_samples_also_expire_without_following_links() {
    use std::time::{Duration, SystemTime};
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("review");
    let now = 1_800_000_000;
    let store = ReviewStore::open(home.path().join("review.json"), root.clone()).unwrap();
    sample(&root, "r-1-2-3", now);
    fs::write(root.join("r-1-2-3/record.json"), "{broken").unwrap();
    let abandoned = root.join(".pending-r-1-2-4");
    fs::create_dir(&abandoned).unwrap();
    for dir in [root.join("r-1-2-3"), abandoned.clone()] {
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(now - 7 * 86400);
        // Directory handles on Windows need special flags; adjust via a child
        // file only on Unix, where the collector currently operates.
        #[cfg(unix)]
        fs::File::open(dir)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(old))
            .unwrap();
        #[cfg(not(unix))]
        let _ = (dir, old);
    }
    assert!(store.list(now).unwrap().is_empty());
    #[cfg(unix)]
    {
        assert!(!root.join("r-1-2-3").exists());
        assert!(!abandoned.exists());
        use std::os::unix::fs::symlink;
        let outside = home.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), "untouched").unwrap();
        symlink(&outside, root.join("r-1-2-5")).unwrap();
        assert!(store.list(now).unwrap().is_empty());
        assert!(outside.join("keep").exists());
        sample(&root, "r-1-2-6", now);
        let item = store.list(now).unwrap().remove(0);
        symlink(outside.join("keep"), root.join("r-1-2-6/audio.wav")).unwrap();
        assert!(store.audio(&item.id, &item.revision, now).is_err());
    }
}
