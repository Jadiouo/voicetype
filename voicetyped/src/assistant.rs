//! Context and learning orchestration. Only text delivered by this daemon is
//! eligible for automatic correction feedback; screen context stays in memory.
use std::collections::{BTreeSet, VecDeque};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::personalization::{ContextSnapshot, LearningOutcome, Personalization, RejectionReason};
use crate::refine::{Mode, Refiner};
use anyhow::Result;
use serde_json::{json, Value};

struct Delivered {
    session: u64,
    scope: ContextSnapshot,
    text: String,
    at: Instant,
}

#[derive(Debug, thiserror::Error)]
#[error("correction does not match a recent delivered dictation")]
pub struct CorrectionAttributionError;

pub struct Assistant {
    learned: Mutex<Personalization>,
    recent: Mutex<VecDeque<Delivered>>,
    manual_context: Mutex<(String, String, Instant)>,
    refiner: Option<Refiner>,
    spelling: Option<crate::csc::CscClient>,
}

impl Assistant {
    pub fn load() -> Result<Self> {
        let path = std::env::var_os("VOICETYPE_LEARNING_FILE")
            .map(std::path::PathBuf::from)
            .map(Ok)
            .unwrap_or_else(Personalization::default_path)?;
        let learned = Personalization::load(&path).unwrap_or_else(|error| {
            tracing::warn!(
                "personal terminology persistence unavailable; using temporary memory: {error}"
            );
            Personalization::memory()
        });
        let mut assistant = Self::new(learned, Refiner::from_env());
        assistant.spelling = crate::csc::CscClient::from_env();
        Ok(assistant)
    }

    pub(crate) fn new(learned: Personalization, refiner: Option<Refiner>) -> Self {
        Self {
            learned: Mutex::new(learned),
            recent: Mutex::new(VecDeque::new()),
            manual_context: Mutex::new((String::new(), String::new(), Instant::now())),
            refiner,
            spelling: None,
        }
    }

    pub fn process(&self, text: &str, scope: &ContextSnapshot, mode: Option<&str>) -> String {
        self.process_with_terms(text, scope, mode, &[], &[])
    }

    pub fn process_with_terms(
        &self,
        text: &str,
        scope: &ContextSnapshot,
        mode: Option<&str>,
        dictionary_terms: &[String],
        canonical_names: &[String],
    ) -> String {
        let mut scope = scope.bounded();
        {
            let manual = self
                .manual_context
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if manual.2.elapsed() < Duration::from_secs(1800)
                && (manual.0.is_empty() || manual.0 == scope.program)
            {
                // Explicitly supplied context must survive a full surrounding
                // text window; prioritize it before applying the shared cap.
                if !manual.1.is_empty() {
                    scope.text = format!("{}\n{}", manual.1, scope.text);
                }
            }
        }
        let scope = scope.bounded();
        let (text, mut terms) = {
            let learned = self.learned.lock().unwrap_or_else(|e| e.into_inner());
            (
                learned.apply_preserving_names(text, &scope, canonical_names),
                learned.candidates(&scope),
            )
        };
        terms.extend_from_slice(dictionary_terms);
        if mode == Some("off") {
            return text;
        }
        if let Some(spelling) = &self.spelling {
            return spelling.correct(&text, &terms);
        }
        let terms = prioritized_terms(terms);
        let Some(refiner) = &self.refiner else {
            return text;
        };
        // Unverified previous ASR output is retained only for correction
        // attribution. It must not become evidence for another name correction.
        let context: String = format!("{}\n{}", scope.selected_text, scope.text)
            .chars()
            .take(4096)
            .collect();
        let started = Instant::now();
        let outcome = match mode {
            Some("clean") => refiner
                .with_mode(Mode::Clean)
                .refine_detailed(&text, &context, &terms),
            Some("faithful") => refiner
                .with_mode(Mode::Faithful)
                .refine_detailed(&text, &context, &terms),
            _ => refiner.refine_detailed(&text, &context, &terms),
        };
        tracing::info!(status = ?outcome.status, edits = outcome.edits,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "local text refinement completed");
        outcome.text
    }

    pub fn remember(&self, session: u64, scope: ContextSnapshot, text: String) {
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        recent.push_back(Delivered {
            session,
            scope: scope.bounded(),
            text,
            at: Instant::now(),
        });
        while recent.len() > 16 {
            recent.pop_front();
        }
    }

    pub fn correction(
        &self,
        session: u64,
        program: &str,
        context_id: &str,
        before: &str,
        after: &str,
        confirmed: bool,
    ) -> Result<bool> {
        Ok(self
            .correction_detailed(session, program, context_id, before, after, confirmed)?
            .changed)
    }

    pub fn correction_detailed(
        &self,
        session: u64,
        program: &str,
        context_id: &str,
        before: &str,
        after: &str,
        confirmed: bool,
    ) -> Result<LearningOutcome> {
        let mut scope = {
            let recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
            let Some(prev) = recent.iter().rev().find(|p| {
                p.session == session
                    && p.scope.program == program
                    && p.scope.context_id == context_id
                    && p.text == before
                    && p.at.elapsed() < Duration::from_secs(300)
            }) else {
                return Err(CorrectionAttributionError.into());
            };
            prev.scope.clone()
        };
        // Explicit UI feedback promises application scope. Unknown programs
        // must never silently turn that action into a global substitution.
        if confirmed && scope.program.is_empty() {
            return Ok(LearningOutcome::rejected(RejectionReason::MissingContext));
        }
        let mut learned = self.learned.lock().unwrap_or_else(|e| e.into_inner());
        if confirmed {
            // Explicit user confirmation persists for this application; an IC
            // nonce is intentionally not a durable document identifier.
            scope.context_id.clear();
            scope.selected_text = after.chars().take(512).collect();
            learned.confirm_correction_detailed(before, after, &scope)
        } else {
            learned.observe_correction_detailed(before, after, &scope, session)
        }
    }

    pub fn learn(&self, wrong: &str, right: &str, program: &str) -> Result<Value> {
        let scope = ContextSnapshot {
            program: program.into(),
            ..Default::default()
        };
        let mut learned = self.learned.lock().unwrap_or_else(|e| e.into_inner());
        learned.learn(wrong, right, &scope)?;
        Ok(
            json!({"learned":true,"wrong":wrong,"right":right,"program":program,
            "persisted":learned.is_persistent()}),
        )
    }

    pub fn list(&self) -> Result<Value> {
        let learned = self.learned.lock().unwrap_or_else(|e| e.into_inner());
        Ok(Value::Array(
            learned
                .list()
                .iter()
                .map(|rule| {
                    json!({
                        "wrong":rule.wrong,"right":rule.right,"program":rule.program,
                        "context_id":rule.context_id,"confirmed":rule.confirmed,
                        "active":rule.active(),"observations":rule.observations()
                    })
                })
                .collect(),
        ))
    }

    pub fn forget(&self, wrong: &str, context_id: Option<&str>) -> Result<Value> {
        let removed = self
            .learned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .forget(wrong, context_id)?;
        Ok(json!({"removed":removed}))
    }

    pub fn set_context(&self, text: &str, program: &str) -> Value {
        let text: String = text.chars().take(3072).collect();
        let n = text.chars().count();
        *self
            .manual_context
            .lock()
            .unwrap_or_else(|e| e.into_inner()) =
            (program.chars().take(128).collect(), text, Instant::now());
        json!({"context_chars":n,"expires_in_seconds":1800})
    }
}

fn prioritized_terms(mut terms: Vec<String>) -> Vec<String> {
    // Baseline technical terms are fallback hints. Preserve selected
    // and contextual term priority instead of sorting them behind ASCII words.
    terms.extend(
        ["maybe", "GitHub", "push", "commit", "branch", "rebase"]
            .into_iter()
            .map(str::to_owned),
    );
    let mut seen = BTreeSet::new();
    terms.retain(|term| seen.insert(term.to_ascii_lowercase()));
    terms.truncate(64);
    terms
}

/// A single executable path, never a shell command. Fail closed on unavailable
/// accessibility providers. Runs in spawn_blocking while the user is speaking.
pub fn desktop_context_enabled() -> bool {
    screen_context_helper(
        std::env::var_os("VOICETYPE_ENABLE_SCREEN_CONTEXT").as_deref(),
        std::env::var_os("VOICETYPE_CONTEXT_HELPER").as_deref(),
    ).is_some()
}

fn screen_context_opt_in(value: Option<&std::ffi::OsStr>) -> bool {
    value == Some(std::ffi::OsStr::new("1"))
}

pub fn desktop_context() -> String {
    desktop_context_with(
        std::env::var_os("VOICETYPE_ENABLE_SCREEN_CONTEXT").as_deref(),
        std::env::var_os("VOICETYPE_CONTEXT_HELPER").as_deref(),
    )
}

fn screen_context_helper<'a>(enabled: Option<&std::ffi::OsStr>, helper: Option<&'a std::ffi::OsStr>) -> Option<&'a std::ffi::OsStr> {
    helper.filter(|path| screen_context_opt_in(enabled) && !path.is_empty())
}

fn desktop_context_with(enabled: Option<&std::ffi::OsStr>, helper: Option<&std::ffi::OsStr>) -> String {
    let Some(helper) = screen_context_helper(enabled, helper) else {
        return String::new();
    };
    let Ok(mut child) = Command::new(helper)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
    else {
        return String::new();
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return String::new();
            }
            Ok(None) if start.elapsed() < Duration::from_millis(500) => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return String::new();
            }
        }
    }
    let mut out = String::new();
    if let Some(stdout) = child.stdout.take() {
        let _ = stdout.take(16384).read_to_string(&mut out);
    }
    serde_json::from_str::<Value>(&out)
        .ok()
        .and_then(|v| v["text"].as_str().map(|s| s.chars().take(3072).collect()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_context_requires_explicit_opt_in() {
        use std::ffi::OsStr;
        for value in [None, Some(OsStr::new("")), Some(OsStr::new("0")), Some(OsStr::new("true"))] {
            assert!(!screen_context_opt_in(value));
        }
        assert!(screen_context_opt_in(Some(OsStr::new("1"))));
    }

    #[test]
    fn configured_screen_helper_does_not_run_without_opt_in() {
        use std::ffi::OsStr;
        use std::os::unix::fs::PermissionsExt;
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("voicetype-helper-test-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let helper = dir.join("helper");
        let marker = dir.join("helper.marker");
        std::fs::write(&helper, "#!/bin/sh\nprintf ran > \"$0.marker\"\nprintf '%s' '{\"text\":\"explicitly enabled\"}'\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Exercise the same gating/executor used by desktop_context(), with
        // configuration passed explicitly so parallel tests never mutate env.
        for enabled in [None, Some(OsStr::new("0"))] {
            assert_eq!(desktop_context_with(enabled, Some(helper.as_os_str())), "");
            assert!(!marker.exists(), "disabled helper was executed");
        }
        assert_eq!(desktop_context_with(Some(OsStr::new("1")), Some(helper.as_os_str())), "explicitly enabled");
        assert!(marker.exists(), "positive control did not execute configured helper");
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn scope(id: &str) -> ContextSnapshot {
        ContextSnapshot {
            program: "browser".into(),
            context_id: id.into(),
            ..Default::default()
        }
    }

    fn assistant() -> Assistant {
        Assistant::new(Personalization::memory(), None)
    }

    #[test]
    fn confirmed_name_correction_does_not_merge_distinct_roles() {
        let assistant = assistant();
        let initial = scope("first");
        let before = "林志豪老師剛才提到的方向，我想先整理成一頁摘要。";
        let after = "林智豪老師剛才提到的方向，我想先整理成一頁摘要。";
        assistant.remember(51, initial, before.into());
        assert!(assistant.correction(51, "browser", "first", before, after, true).unwrap());
        let current = ContextSnapshot {
            text: "聯絡人：行政窗口林志豪；研究指導林智豪老師。".into(),
            ..scope("later")
        };
        // The recognizer used the same spelling for two roles. Only the first
        // is correct; a term-wide replacement silently destroys that correct name.
        let heard = "窗口叫林志豪，老師叫林志豪記性的時候，不要填成同一個人。";
        assert_eq!(assistant.process(heard, &current, Some("off")), heard);
    }

    #[test]
    fn explicit_rule_abstains_when_context_contains_both_spellings() {
        let assistant = assistant();
        assistant.learn("陳博宇", "陳柏宇", "browser").unwrap();
        let current = ContextSnapshot {
            text: "窗口陳博宇與老師陳柏宇是不同人。".into(),
            ..scope("later")
        };
        let heard = "請找窗口陳博宇。";
        assert_eq!(assistant.process(heard, &current, Some("off")), heard);
    }

    #[test]
    fn learned_rule_preserves_distinct_spellings_already_in_the_utterance() {
        let assistant = assistant();
        assistant.learn("陳博宇", "陳柏宇", "browser").unwrap();
        let heard = "窗口是陳博宇，老師是陳柏宇。";
        assert_eq!(assistant.process(heard, &scope("later"), Some("off")), heard);
    }

    #[test]
    fn confirmed_rule_does_not_override_an_explicit_source_spelling_in_context() {
        let assistant = assistant();
        assistant.learn("陳博宇", "陳柏宇", "browser").unwrap();
        let current = ContextSnapshot {
            text: "窗口姓名登記為陳博宇，請沿用原名。".into(),
            ..scope("later")
        };
        assert_eq!(assistant.process("請找陳博宇。", &current, Some("off")), "請找陳博宇。");
    }

    #[test]
    fn single_unambiguous_spelling_still_applies_but_repetitions_abstain() {
        let assistant = assistant();
        assistant.learn("mabe", "maybe", "browser").unwrap();
        let current = scope("later");
        assert_eq!(assistant.process("這個mabe是對的", &current, Some("off")), "這個maybe是對的");
        // A deliberate recall tradeoff: no per-occurrence evidence for repeats.
        assert_eq!(assistant.process("mabe 和 mabe", &current, Some("off")), "mabe 和 mabe");
    }

    #[test]
    fn boundary_spacing_does_not_hide_a_confirmed_ascii_spelling_observation() {
        use crate::personalization::LearningStatus;
        let assistant = assistant();
        let current = scope("field-a");
        let before = "這個東西mabe是對的，但我想先確認一下再決定。";
        let after = "這個東西 maybe 是對的，但我想先確認一下再決定。";
        for session in [11, 12] {
            assistant.remember(session, current.clone(), before.into());
            let outcome = assistant
                .correction_detailed(session, "browser", "field-a", before, after, false)
                .unwrap();
            assert_eq!(
                outcome.status,
                if session == 11 {
                    LearningStatus::Pending
                } else {
                    LearningStatus::Activated
                }
            );
            if session == 11 {
                assert_eq!(assistant.process(before, &current, Some("off")), before);
            }
        }
        // Formatting was normalized for comparison only, not for delivery.
        assert_eq!(
            assistant.process(before, &current, Some("off")),
            "這個東西maybe是對的，但我想先確認一下再決定。"
        );
        assert_eq!(
            assistant.process(before, &scope("different-field"), Some("off")),
            before
        );
    }

    #[test]
    fn sentence_feedback_does_not_learn_chinese_word_reordering() {
        use crate::personalization::{LearningStatus, RejectionReason};
        let assistant = assistant();
        let before = "請幫我把剛才討論的重點寫下來，我等一下會自己檢查。";
        let after = "請幫我把剛才討論的重點寫下來，等一下我會自己檢查。";
        assistant.remember(5, scope("field-a"), before.into());
        let outcome = assistant
            .correction_detailed(5, "browser", "field-a", before, after, false)
            .unwrap();
        assert_eq!(outcome.status, LearningStatus::Rejected);
        assert_eq!(outcome.reason, Some(RejectionReason::NotSingleTerm));
        assert_eq!(assistant.list().unwrap(), json!([]));
    }

    #[test]
    fn spacing_fallback_never_learns_phrase_formatting_or_multiple_edits() {
        use crate::personalization::LearningStatus;
        let assistant = assistant();
        for (index, (before, after)) in [
            (
                "這個mabe可以push到gthub上面",
                "這個 maybe 可以 push 到 GitHub 上面",
            ),
            ("這個side project可以完成", "這個 sideproject 可以完成"),
            ("這個sideproject可以完成", "這個 side project 可以完成"),
            ("這個maybe是對的", "這個 maybe 是對的"),
            ("這個mabe用32個參數", "這個 maybe 用三十二個參數"),
            ("這個mabe對，後面再看", "這個 maybe 對。後面再看"),
            ("檔案/srv/mabe保持不變", "檔案 /srv/maybe 保持不變"),
            ("這個mabe_value保持不變", "這個 maybe_value 保持不變"),
        ]
        .into_iter()
        .enumerate()
        {
            let session = 100 + index as u64;
            assistant.remember(session, scope("field-a"), before.into());
            let outcome = assistant
                .correction_detailed(session, "browser", "field-a", before, after, false)
                .unwrap();
            assert_eq!(
                outcome.status,
                LearningStatus::Rejected,
                "{before:?} -> {after:?}"
            );
        }
        assert_eq!(assistant.list().unwrap(), json!([]));
    }

    #[test]
    fn spacing_fallback_cannot_borrow_an_unchanged_word_to_validate_a_path_edit() {
        use crate::personalization::LearningStatus;
        let assistant = assistant();
        let before = "這個mabe用作標籤，maybe是另一個字，文件/srv/mabe保持不變";
        let after = "這個 mabe 用作標籤，maybe是另一個字，文件/srv/maybe保持不變";
        assistant.remember(15, scope("field-a"), before.into());
        let outcome = assistant
            .correction_detailed(15, "browser", "field-a", before, after, false)
            .unwrap();
        assert_eq!(outcome.status, LearningStatus::Rejected);
        assert_eq!(assistant.list().unwrap(), json!([]));
    }

    #[test]
    fn sentence_feedback_never_learns_paths_literals_identifiers_or_options() {
        use crate::personalization::LearningStatus;
        let assistant = assistant();
        for (index, (before, after)) in [
            ("文件/srv/mabe保持不變", "文件/srv/maybe保持不變"),
            ("保留`mabe`這個字", "保留`maybe`這個字"),
            ("這個mabe_value保持不變", "這個maybe_value保持不變"),
            ("使用--mabe參數", "使用--maybe參數"),
            ("保留'mabe'字串", "保留'maybe'字串"),
            ("開啟mabe.json", "開啟maybe.json"),
        ]
        .into_iter()
        .enumerate()
        {
            let session = 200 + index as u64;
            assistant.remember(session, scope("field-a"), before.into());
            let outcome = assistant
                .correction_detailed(session, "browser", "field-a", before, after, false)
                .unwrap();
            assert_eq!(
                outcome.status,
                LearningStatus::Rejected,
                "{before:?} -> {after:?}"
            );
        }
        assert_eq!(assistant.list().unwrap(), json!([]));
    }

    #[test]
    fn sentence_extraction_abstains_on_transposed_name_but_explicit_pair_remains_available() {
        use crate::personalization::LearningStatus;
        let assistant = assistant();
        let before = "我要請王安以老師幫忙。";
        let after = "我要請王以安老師幫忙。";
        assistant.remember(6, scope("field-a"), before.into());
        let outcome = assistant
            .correction_detailed(6, "browser", "field-a", before, after, true)
            .unwrap();
        assert_eq!(outcome.status, LearningStatus::Rejected);
        assistant.learn("王安以", "王以安", "browser").unwrap();
        assert_eq!(
            assistant.process(before, &scope("field-a"), Some("off")),
            after
        );
    }

    #[test]
    fn strict_sentence_case_preference_is_still_learned() {
        use crate::personalization::LearningStatus;
        let assistant = assistant();
        assistant.remember(7, scope("field-a"), "推到github上面".into());
        let outcome = assistant
            .correction_detailed(
                7,
                "browser",
                "field-a",
                "推到github上面",
                "推到GitHub上面",
                true,
            )
            .unwrap();
        assert_eq!(outcome.status, LearningStatus::Confirmed);
        assert_eq!(
            assistant.process("github 和 github", &scope("later"), Some("off")),
            "GitHub 和 GitHub"
        );
    }

    #[test]
    fn repeated_capitalization_preference_preserves_literals_and_paths() {
        let assistant = assistant();
        assistant.learn("github", "GitHub", "browser").unwrap();
        let current = ContextSnapshot {
            text: "github GitHub".into(),
            ..scope("later")
        };
        assert_eq!(assistant.process("github 和 github", &current, Some("off")), "GitHub 和 GitHub");
        assert_eq!(assistant.process("github 和 /tmp/github 和 github.com", &current, Some("off")),
            "GitHub 和 /tmp/github 和 github.com");
        assert_eq!(assistant.process("`github` 和 github", &current, Some("off")), "`github` 和 github");
    }

    fn observe_twice(assistant: &Assistant, context_id: &str) {
        for session in [1, 2] {
            assistant.remember(session, scope(context_id), "推到gthub上面".into());
            assert!(assistant
                .correction(
                    session,
                    "browser",
                    context_id,
                    "推到gthub上面",
                    "推到GitHub上面",
                    false
                )
                .unwrap());
        }
    }

    #[test]
    fn correction_attribution_rejects_forged_stale_and_mismatched_deliveries() {
        let assistant = assistant();
        let before = "這個mabe是對的";
        let after = "這個maybe是對的";
        assistant.remember(7, scope("field-a"), before.into());
        for (session, program, context_id, supplied_before) in [
            (8, "browser", "field-a", before),
            (7, "editor", "field-a", before),
            (7, "browser", "field-b", before),
            (7, "browser", "field-a", "forged original"),
        ] {
            assert!(assistant
                .correction(session, program, context_id, supplied_before, after, true)
                .is_err());
        }
        assistant.recent.lock().unwrap().front_mut().unwrap().at =
            Instant::now() - Duration::from_secs(301);
        assert!(assistant
            .correction(7, "browser", "field-a", before, after, true)
            .is_err());
        assert_eq!(assistant.list().unwrap(), json!([]));
    }

    #[test]
    fn observations_require_separately_delivered_sessions() {
        let assistant = assistant();
        let before = "這個mabe是對的";
        let after = "這個maybe是對的";
        let context = scope("field-a");
        assistant.remember(10, context.clone(), before.into());
        assert!(assistant
            .correction(10, "browser", "field-a", before, after, false)
            .unwrap());
        assert!(!assistant
            .correction(10, "browser", "field-a", before, after, false)
            .unwrap());
        assert_eq!(assistant.list().unwrap()[0]["observations"], 1);
        assert_eq!(assistant.process(before, &context, None), before);
        assert!(assistant
            .correction(11, "browser", "field-a", before, after, false)
            .is_err());
        assistant.remember(11, context.clone(), before.into());
        assert!(assistant
            .correction(11, "browser", "field-a", before, after, false)
            .unwrap());
        assert_eq!(assistant.process(before, &context, None), after);
        assert_eq!(assistant.list().unwrap()[0]["observations"], 2);
    }

    #[test]
    fn explicit_confirmation_persists_for_the_application_across_restart() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "voicetype-assistant-test-{}-{unique}",
            std::process::id()
        ));
        let path = dir.join("memory.json");
        let before = "我想問陳博宇教授的研究方向";
        let after = "我想問陳柏宇教授的研究方向";
        {
            let assistant = Assistant::new(Personalization::load(&path).unwrap(), None);
            assistant.remember(20, scope("ephemeral-field"), before.into());
            let outcome = assistant
                .correction_detailed(20, "browser", "ephemeral-field", before, after, true)
                .unwrap();
            assert_eq!(
                outcome.status,
                crate::personalization::LearningStatus::Confirmed
            );
            assert_eq!(outcome.wrong.as_deref(), Some("陳博宇"));
            assert_eq!(outcome.right.as_deref(), Some("陳柏宇"));
            assert!(outcome.persisted && outcome.changed);
            assert_eq!(assistant.list().unwrap()[0]["context_id"], "");
        }
        let restarted = Assistant::new(Personalization::load(&path).unwrap(), None);
        assert_eq!(restarted.process(before, &scope("new-field"), None), after);
        let other_app = ContextSnapshot {
            program: "editor".into(),
            context_id: "new-field".into(),
            ..Default::default()
        };
        assert_eq!(restarted.process(before, &other_app, None), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn independent_manual_context_corroborates_cross_field_learning_and_expires() {
        let assistant = assistant();
        observe_twice(&assistant, "training-field");
        let before = "推到gthub上面";
        let new_field = scope("new-field");
        assert_eq!(assistant.process(before, &new_field, None), before);
        assistant.set_context("GitHub 專案", "editor");
        assert_eq!(assistant.process(before, &new_field, None), before);
        assistant.set_context("GitHub 專案", "browser");
        assert_eq!(
            assistant.process(before, &new_field, None),
            "推到GitHub上面"
        );
        let full_field = ContextSnapshot {
            text: "中".repeat(4096),
            ..new_field.clone()
        };
        assert_eq!(
            assistant.process(before, &full_field, None),
            "推到GitHub上面"
        );
        assistant.manual_context.lock().unwrap().2 = Instant::now() - Duration::from_secs(1801);
        assert_eq!(assistant.process(before, &new_field, None), before);
    }

    #[test]
    fn recent_asr_and_generated_candidates_never_corroborate_learned_rules() {
        let assistant = assistant();
        observe_twice(&assistant, "training-field");
        let new_field = scope("new-field");
        assistant.remember(3, new_field.clone(), "GitHub".into());
        // Both the learned candidate list and recent dictation contain GitHub,
        // but neither is independently obtained evidence for this new field.
        assert_eq!(
            assistant.process("推到gthub上面", &new_field, None),
            "推到gthub上面"
        );
        let real_context = ContextSnapshot {
            text: "GitHub 專案".into(),
            ..new_field
        };
        assert_eq!(
            assistant.process("推到gthub上面", &real_context, None),
            "推到GitHub上面"
        );
    }

    #[test]
    fn term_cap_retains_selected_chinese_names_before_defaults() {
        let mut contextual = vec!["陳柏宇".to_owned(), "GITHUB".to_owned()];
        contextual.extend((0..70).map(|i| format!("aaaa{i}")));
        let terms = prioritized_terms(contextual);
        assert_eq!(terms.len(), 64);
        assert_eq!(terms[0], "陳柏宇");
        assert_eq!(terms[1], "GITHUB");
        assert_eq!(
            terms
                .iter()
                .filter(|t| t.eq_ignore_ascii_case("github"))
                .count(),
            1
        );
        assert!(!terms.contains(&"maybe".to_owned()));
    }

    #[test]
    fn detailed_learning_distinguishes_pending_duplicate_activation_and_confirmation() {
        use crate::personalization::LearningStatus;
        let assistant = assistant();
        let before = "這個mabe是對的";
        let after = "這個maybe是對的";
        assistant.remember(1, scope("field-a"), before.into());
        let pending = assistant
            .correction_detailed(1, "browser", "field-a", before, after, false)
            .unwrap();
        assert_eq!(pending.status, LearningStatus::Pending);
        assert_eq!(pending.observations, 1);
        assert!(pending.changed);
        assert!(!pending.persisted);
        assert_eq!(pending.wrong.as_deref(), Some("mabe"));
        assert_eq!(pending.right.as_deref(), Some("maybe"));
        let duplicate = assistant
            .correction_detailed(1, "browser", "field-a", before, after, false)
            .unwrap();
        assert_eq!(duplicate.status, LearningStatus::Duplicate);
        assert_eq!(duplicate.observations, 1);
        assert!(!duplicate.changed);
        assistant.remember(2, scope("field-a"), before.into());
        let activated = assistant
            .correction_detailed(2, "browser", "field-a", before, after, false)
            .unwrap();
        assert_eq!(activated.status, LearningStatus::Activated);
        assert_eq!(activated.observations, 2);
        assert_eq!(assistant.process(before, &scope("field-a"), None), after);
        let confirmed = assistant
            .correction_detailed(2, "browser", "field-a", before, after, true)
            .unwrap();
        assert_eq!(confirmed.status, LearningStatus::Confirmed);
        let already = assistant
            .correction_detailed(2, "browser", "field-a", before, after, true)
            .unwrap();
        assert_eq!(already.status, LearningStatus::AlreadyKnown);
        assert!(!already.changed);
    }

    #[test]
    fn explicit_nonterm_rejection_exposes_no_sentence_and_changes_no_rules() {
        use crate::personalization::{LearningStatus, RejectionReason};
        let assistant = assistant();
        let before = "這段內容包含私人研究計畫";
        assistant.remember(1, scope("field-a"), before.into());
        let outcome = assistant
            .correction_detailed(
                1,
                "browser",
                "field-a",
                before,
                "算了還是先離開，之後重新討論。",
                true,
            )
            .unwrap();
        assert_eq!(outcome.status, LearningStatus::Rejected);
        assert_eq!(outcome.reason, Some(RejectionReason::NotSingleTerm));
        assert!(outcome.wrong.is_none() && outcome.right.is_none());
        assert!(!outcome.changed && !outcome.persisted);
        assert_eq!(assistant.list().unwrap(), json!([]));
    }

    #[test]
    fn explicit_ui_confirmation_with_unknown_application_never_learns_globally() {
        use crate::personalization::LearningStatus;
        let assistant = assistant();
        let unknown = ContextSnapshot {
            context_id: "unknown-app-field".into(),
            ..Default::default()
        };
        assistant.remember(1, unknown, "mabe".into());
        let outcome = assistant
            .correction_detailed(1, "", "unknown-app-field", "mabe", "maybe", true)
            .unwrap();
        assert_eq!(outcome.status, LearningStatus::Rejected);
        assert_eq!(outcome.reason, Some(RejectionReason::MissingContext));
        assert!(!outcome.changed);
        assert_eq!(assistant.list().unwrap(), json!([]));
        // The deliberate CLI/API global operation remains available.
        let learned = assistant.learn("mabe", "maybe", "").unwrap();
        assert_eq!(learned["persisted"], false);
        assert_eq!(
            assistant.process("mabe", &scope("any-app-field"), None),
            "maybe"
        );
    }
}
