use std::fs;
use voicetype_app_core::vocabulary::Vocabulary;

#[test]
fn edit_preview_reopen_and_restore_preserve_existing_vocabulary() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vocab.toml");
    let original = "# user note\nterms=[\"commit\"]\nfuture=17\n\n[[entry]]\n# rule note\nwrong=[\"git hub\"]\nright=\"GitHub\"\nextra=true\n";
    fs::write(&path, original).unwrap();
    let mut editor = Vocabulary::open(path.clone()).unwrap();
    let version = editor.snapshot().revision;
    let saved = editor
        .put(
            &version,
            Some(0),
            vec!["git hub".into(), "geeho".into()],
            "GitHub".into(),
        )
        .unwrap();
    assert_eq!(saved.entries[0].wrong, ["git hub", "geeho"]);
    let document = fs::read_to_string(&path).unwrap();
    for kept in ["# user note", "# rule note", "future=17", "extra=true"] {
        assert!(document.contains(kept), "lost {kept}");
    }
    let reopened = Vocabulary::open(path.clone()).unwrap();
    assert_eq!(
        reopened
            .preview("先 push 到 GEEHO，coming soon；`geeho`、geeho.com 保留。")
            .unwrap(),
        "先 push 到 GitHub，coming soon；`geeho`、geeho.com 保留。"
    );
    editor.restore(&saved.revision).unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), original);
}

#[test]
fn import_is_explicit_preserves_exact_bytes_and_never_overwrites_existing_app_data() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("legacy.toml");
    let destination = root.path().join("preview/vocab.toml");
    let original = "# existing dictionary\nfuture=3\nentry=[]\n";
    fs::write(&source, original).unwrap();
    let mut editor = Vocabulary::open(destination.clone()).unwrap();
    assert!(!destination.exists());
    editor
        .import_existing(&editor.snapshot().revision, &source)
        .unwrap();
    assert_eq!(fs::read_to_string(&destination).unwrap(), original);
    assert!(editor
        .import_existing(&editor.snapshot().revision, &source)
        .is_err());
    editor
        .set_terms(
            &editor.snapshot().revision,
            vec!["GitHub".into(), "commit".into(), "push".into()],
        )
        .unwrap();
    assert_eq!(
        Vocabulary::open(destination).unwrap().snapshot().terms,
        ["GitHub", "commit", "push"]
    );
    assert_eq!(fs::read_to_string(source).unwrap(), original);
}

#[test]
fn protected_names_reject_opencc_alias_collisions_and_preserve_inline_rules() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vocab.toml");
    fs::write(
        &path,
        "# names note\nnames=[]\nentry=[{wrong=['git hub'],right='GitHub',future=9}]\n",
    )
    .unwrap();
    let mut editor = Vocabulary::open(path.clone()).unwrap();
    let saved = editor
        .set_names(
            &editor.snapshot().revision,
            vec!["台積電".into(), "游錫堃".into()],
        )
        .unwrap();
    assert_eq!(
        editor.preview("台積電與臺積電，游錫堃 git hub").unwrap(),
        "台積電與台積電，游錫堃 GitHub"
    );
    assert!(editor
        .put(&saved.revision, None, vec!["臺積電".into()], "台泥".into())
        .is_err());
    editor
        .put(
            &saved.revision,
            Some(0),
            vec!["geeho".into()],
            "GitHub".into(),
        )
        .unwrap();
    assert!(fs::read_to_string(&path).unwrap().contains("future=9"));
    editor.delete(&editor.snapshot().revision, 0).unwrap();
    assert!(Vocabulary::open(path)
        .unwrap()
        .snapshot()
        .entries
        .is_empty());
}

#[test]
fn stale_windows_and_external_edits_cannot_overwrite_a_newer_vocabulary() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vocab.toml");
    let mut first = Vocabulary::open(path.clone()).unwrap();
    let mut second = Vocabulary::open(path.clone()).unwrap();
    let stale = second.snapshot().revision;
    let saved = first
        .put(
            &first.snapshot().revision,
            None,
            vec!["geeho".into()],
            "GitHub".into(),
        )
        .unwrap();
    assert!(second
        .put(&stale, None, vec!["pushh".into()], "push".into())
        .is_err());
    assert!(first.delete(&stale, 0).is_err());
    fs::write(&path, "# external edit\nentry=[]\n").unwrap();
    assert!(first.delete(&saved.revision, 0).is_err());
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "# external edit\nentry=[]\n"
    );
}

#[test]
fn invalid_files_rules_and_busy_writers_preserve_the_last_good_bytes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vocab.toml");
    for original in [b"invalid = [".as_slice(), &[b'#'; 128 * 1024 + 1]] {
        fs::write(&path, original).unwrap();
        assert!(Vocabulary::open(path.clone()).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }
    fs::write(&path, "entry=[]\n").unwrap();
    let mut editor = Vocabulary::open(path.clone()).unwrap();
    let revision = editor.snapshot().revision;
    for (wrong, right) in [
        (vec![], "GitHub"),
        (vec!["a\nb".into()], "GitHub"),
        (vec!["geeho".into()], " "),
    ] {
        assert!(editor.put(&revision, None, wrong, right.into()).is_err());
    }
    assert!(editor.set_names(&revision, vec!["X".into()]).is_err());
    assert!(editor
        .set_terms(&revision, vec!["word".into(); 257])
        .is_err());
    assert!(editor.preview(&"x".repeat(16385)).is_err());
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("toml.lock"))
        .unwrap();
    lock.lock().unwrap();
    assert!(editor
        .put(&revision, None, vec!["geeho".into()], "GitHub".into())
        .is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "entry=[]\n");
    assert!(!path.with_extension("toml.bak").exists());
    drop(lock);
    let saved = editor
        .put(&revision, None, vec!["geeho".into()], "GitHub".into())
        .unwrap();
    assert!(editor
        .put(&saved.revision, None, vec!["GEEHO".into()], "Other".into())
        .is_err());
    assert_eq!(Vocabulary::open(path).unwrap().snapshot().entries.len(), 1);
}
