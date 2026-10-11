//! Opt-in release probe: install the exact package and talk to its real CPU model.
use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use voicetype_app_core::{assets::AssetManifest, spelling_install::SpellingInstaller};
use voicetype_text::spelling::OwnedSpelling;

#[test]
#[ignore = "requires a freshly built target/spelling CPU release bundle"]
fn installed_bundle_corrects_han_without_dropping_english_or_digits() {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/spelling");
    let manifest: AssetManifest =
        serde_json::from_slice(&fs::read(source.join("manifest.json")).unwrap()).unwrap();
    let profile = tempfile::tempdir().unwrap();
    let paths = SpellingInstaller::new(profile.path().to_owned(), manifest)
        .unwrap()
        .prepare(&source)
        .unwrap();
    assert!(paths.executable.is_file());
    assert!(paths.model.is_file());
    let mut worker =
        OwnedSpelling::start(paths.command().unwrap(), Duration::from_secs(30)).unwrap();
    assert!(worker.is_running().unwrap());
    // A hosted Windows CPU may miss the first 100 ms inference deadline after
    // readiness. That call must fail open; the same owned child must then be
    // able to produce a real correction without changing the deadline.
    let original = "今天新情很好。GitHub 2026";
    let expected = "今天心情很好。GitHub 2026";
    let started = Instant::now();
    let until = started + Duration::from_secs(3);
    let mut attempts = 0;
    let mut slow_calls = 0;
    let mut longest_call = Duration::ZERO;
    loop {
        assert!(worker.is_running().unwrap());
        attempts += 1;
        let call_started = Instant::now();
        let actual = worker.correct(original, &[]);
        let call_elapsed = call_started.elapsed();
        longest_call = longest_call.max(call_elapsed);
        if call_elapsed >= Duration::from_millis(50) {
            slow_calls += 1;
        }
        if actual == expected {
            eprintln!(
                "CSC installed CPU probe: first_fail_open={} warm_correction_attempt={} elapsed_ms={} slow_calls={} longest_call_ms={}",
                attempts > 1,
                attempts,
                started.elapsed().as_millis(),
                slow_calls,
                longest_call.as_millis()
            );
            break;
        }
        assert_eq!(actual, original, "unexpected model rewrite");
        assert!(
            Instant::now() < until,
            "CPU model never corrected after {attempts} attempts within the 3 s test budget; slow_calls={slow_calls}, longest_call_ms={}",
            longest_call.as_millis()
        );
        thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        worker.correct("今天新情很好。GitHub 2026", &["新情".into()]),
        "今天新情很好。GitHub 2026"
    );
    let pid = worker.process_id().unwrap();
    worker.shutdown().unwrap();
    assert!(worker.process_id().is_none());
    #[cfg(target_os = "linux")]
    assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}
