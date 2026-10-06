//! Local, bounded terminology memory. Only correction pairs are persisted: screen
//! text, selections, complete dictations and audio never enter this store.
//!
//! Observations must be finalized edits belonging to an actual delivered
//! dictation (the caller validates that association). Two distinct dictation
//! sessions are required before an observed mapping becomes active. This is a
//! terminology aid, not model training or unrestricted sentence rewriting.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

const MAX_RULES: usize = 512;
const MAX_TERM_CHARS: usize = 64;
const MAX_EVIDENCE: usize = 8;
const MAX_STORE_BYTES: u64 = 2 * 1024 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextSnapshot {
    pub program: String,
    pub context_id: String,
    pub text: String,
    pub selected_text: String,
}

impl ContextSnapshot {
    /// The surrounding-text producer should center text on the cursor before
    /// calling this; the daemon additionally enforces these limits.
    pub fn bounded(&self) -> Self {
        Self {
            program: bounded_text(&self.program, 128),
            context_id: bounded_text(&self.context_id, 160),
            text: bounded_text(&self.text, 4096),
            selected_text: bounded_text(&self.selected_text, 512),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Evidence {
    context_id: String,
    session: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnedRule {
    pub wrong: String,
    pub right: String,
    pub program: String,
    /// Initial scope, also useful when inspecting or deleting a rule.
    pub context_id: String,
    pub confirmed: bool,
    #[serde(default)]
    evidence: Vec<Evidence>,
}

impl LearnedRule {
    pub fn active(&self) -> bool {
        self.confirmed || self.evidence.len() >= 2
    }

    pub fn observations(&self) -> usize {
        self.evidence.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Store {
    version: u32,
    rules: Vec<LearnedRule>,
}

pub struct Personalization {
    path: PathBuf,
    persist: bool,
    rules: Vec<LearnedRule>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningStatus {
    Pending,
    Activated,
    Confirmed,
    AlreadyKnown,
    Duplicate,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    MissingContext,
    NotSingleTerm,
    ConflictingSession,
    MemoryFull,
}

/// Contains only an accepted terminology pair, never the complete input or
/// surrounding context. Safe for a small learning notification/status response.
#[derive(Debug, Clone, Serialize)]
pub struct LearningOutcome {
    pub status: LearningStatus,
    pub wrong: Option<String>,
    pub right: Option<String>,
    pub observations: usize,
    /// False for the corrupt-store fallback: this survives only this process.
    pub persisted: bool,
    /// Compatibility with the original bool response: did the store change?
    pub changed: bool,
    pub reason: Option<RejectionReason>,
}

impl LearningOutcome {
    pub(crate) fn rejected(reason: RejectionReason) -> Self {
        Self {
            status: LearningStatus::Rejected,
            wrong: None,
            right: None,
            observations: 0,
            persisted: false,
            changed: false,
            reason: Some(reason),
        }
    }

    fn for_rule(
        status: LearningStatus,
        rule: &LearnedRule,
        persisted: bool,
        changed: bool,
    ) -> Self {
        Self {
            status,
            wrong: Some(rule.wrong.clone()),
            right: Some(rule.right.clone()),
            observations: rule.observations(),
            persisted,
            changed,
            reason: None,
        }
    }
}

impl Personalization {
    /// Ephemeral fallback after an unreadable/corrupt store. Existing disk data
    /// is never replaced. The caller should explain that learning is temporary.
    pub fn memory() -> Self {
        Self {
            path: PathBuf::new(),
            persist: false,
            rules: Vec::new(),
        }
    }

    pub fn default_path() -> Result<PathBuf> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
                    .map(|p| p.join(".local/share"))
            })
            .context("XDG_DATA_HOME or HOME is required for terminology memory")?;
        Ok(base.join("voicetype/personalization.json"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let rules = match fs::metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).context("read terminology memory metadata"),
            Ok(meta) => {
                ensure!(
                    meta.len() <= MAX_STORE_BYTES,
                    "terminology memory is too large"
                );
                let store: Store =
                    serde_json::from_slice(&fs::read(path)?).context("parse terminology memory")?;
                ensure!(store.version == 1, "unsupported terminology memory version");
                ensure!(store.rules.len() <= MAX_RULES, "too many learned rules");
                for rule in &store.rules {
                    validate_pair(&rule.wrong, &rule.right)?;
                    ensure!(rule.program.chars().count() <= 128, "invalid program scope");
                    ensure!(
                        rule.context_id.chars().count() <= 160,
                        "invalid context scope"
                    );
                    ensure!(
                        rule.evidence.len() <= MAX_EVIDENCE,
                        "too much rule evidence"
                    );
                    let mut unique = BTreeSet::new();
                    for e in &rule.evidence {
                        ensure!(
                            !e.context_id.is_empty() && e.context_id.chars().count() <= 160,
                            "invalid observation scope"
                        );
                        ensure!(
                            unique.insert((&e.context_id, e.session)),
                            "duplicate observation evidence"
                        );
                    }
                }
                store.rules
            }
        };
        Ok(Self {
            path: path.to_owned(),
            persist: true,
            rules,
        })
    }

    pub fn list(&self) -> &[LearnedRule] {
        &self.rules
    }

    pub fn is_persistent(&self) -> bool {
        self.persist
    }

    /// Confirm one compact correction inside a delivered sentence. The same
    /// conservative span extraction is used as for observations, but an explicit
    /// user action enables it immediately. False means no safe term pair was
    /// found; the user can instead supply a complete pair with learn().
    pub fn confirm_correction(
        &mut self,
        before: &str,
        after: &str,
        scope: &ContextSnapshot,
    ) -> Result<bool> {
        Ok(self
            .confirm_correction_detailed(before, after, scope)?
            .changed)
    }

    pub fn confirm_correction_detailed(
        &mut self,
        before: &str,
        after: &str,
        scope: &ContextSnapshot,
    ) -> Result<LearningOutcome> {
        let scope = scope.bounded();
        let pair = correction_pair(before, after, &scope.selected_text);
        let Some((wrong, right)) = pair else {
            return Ok(LearningOutcome::rejected(RejectionReason::NotSingleTerm));
        };
        if let Some(rule) = self.rules.iter().find(|r| {
            r.confirmed
                && r.program == scope.program
                && r.context_id == scope.context_id
                && r.wrong.eq_ignore_ascii_case(&wrong)
                && r.right == right
        }) {
            return Ok(LearningOutcome::for_rule(
                LearningStatus::AlreadyKnown,
                rule,
                self.persist,
                false,
            ));
        }
        if self.rules.len() >= MAX_RULES
            && !self.rules.iter().any(|r| {
                r.wrong.eq_ignore_ascii_case(&wrong)
                    && r.program == scope.program
                    && r.context_id == scope.context_id
            })
        {
            return Ok(LearningOutcome::rejected(RejectionReason::MemoryFull));
        }
        self.learn(&wrong, &right, &scope)?;
        Ok(LearningOutcome::for_rule(
            LearningStatus::Confirmed,
            self.rules.last().expect("learn appends the confirmed rule"),
            self.persist,
            true,
        ))
    }

    /// Explicit user confirmation. An empty scope deliberately creates a global
    /// rule; a program-only scope applies throughout that program. An explicit
    /// rule supersedes conflicting rules in exactly the same scope.
    pub fn learn(&mut self, wrong: &str, right: &str, scope: &ContextSnapshot) -> Result<()> {
        let wrong = wrong.trim();
        let right = right.trim();
        validate_pair(wrong, right)?;
        let scope = scope.bounded();
        let mut next = self.rules.clone();
        next.retain(|r| {
            !(r.wrong.eq_ignore_ascii_case(wrong)
                && r.program == scope.program
                && r.context_id == scope.context_id)
        });
        ensure!(
            next.len() < MAX_RULES,
            "terminology memory is full; forget a rule first"
        );
        next.push(LearnedRule {
            wrong: wrong.to_owned(),
            right: right.to_owned(),
            program: scope.program,
            context_id: scope.context_id,
            confirmed: true,
            evidence: Vec::new(),
        });
        self.commit(next)
    }

    /// Returns true only when new evidence was saved. Repeated notifications
    /// from one session do not promote a rule. The caller must debounce and
    /// finalize edits; this method cannot distinguish typing from a final edit.
    pub fn observe_correction(
        &mut self,
        before: &str,
        after: &str,
        scope: &ContextSnapshot,
        session: u64,
    ) -> Result<bool> {
        Ok(self
            .observe_correction_detailed(before, after, scope, session)?
            .changed)
    }

    pub fn observe_correction_detailed(
        &mut self,
        before: &str,
        after: &str,
        scope: &ContextSnapshot,
        session: u64,
    ) -> Result<LearningOutcome> {
        let scope = scope.bounded();
        if scope.context_id.is_empty() || scope.program.is_empty() {
            return Ok(LearningOutcome::rejected(RejectionReason::MissingContext));
        }
        let Some((wrong, right)) = correction_pair(before, after, &scope.selected_text) else {
            return Ok(LearningOutcome::rejected(RejectionReason::NotSingleTerm));
        };
        if let Some(rule) = self.rules.iter().find(|r| {
            r.confirmed
                && applicable(r, &scope)
                && r.wrong.eq_ignore_ascii_case(&wrong)
                && r.right == right
        }) {
            return Ok(LearningOutcome::for_rule(
                LearningStatus::AlreadyKnown,
                rule,
                self.persist,
                false,
            ));
        }
        let evidence = Evidence {
            context_id: scope.context_id.clone(),
            session,
        };
        let mut next = self.rules.clone();
        // A session contributes at most one observation for a source term,
        // including conflicting interim edits. It cannot vote twice.
        if let Some(rule) = next.iter().find(|r| {
            r.program == scope.program
                && r.wrong.eq_ignore_ascii_case(&wrong)
                && r.evidence.contains(&evidence)
        }) {
            return Ok(if rule.right == right {
                LearningOutcome::for_rule(LearningStatus::Duplicate, rule, self.persist, false)
            } else {
                LearningOutcome::rejected(RejectionReason::ConflictingSession)
            });
        }
        let outcome;
        if let Some(rule) = next.iter_mut().find(|r| {
            !r.confirmed
                && r.program == scope.program
                && r.wrong.eq_ignore_ascii_case(&wrong)
                && r.right == right
        }) {
            let was_active = rule.active();
            if rule.evidence.len() == MAX_EVIDENCE {
                rule.evidence.remove(0);
            }
            rule.evidence.push(evidence);
            outcome = LearningOutcome::for_rule(
                if was_active {
                    LearningStatus::AlreadyKnown
                } else {
                    LearningStatus::Activated
                },
                rule,
                self.persist,
                true,
            );
        } else {
            if next.len() >= MAX_RULES {
                return Ok(LearningOutcome::rejected(RejectionReason::MemoryFull));
            }
            next.push(LearnedRule {
                wrong,
                right,
                program: scope.program,
                context_id: scope.context_id,
                confirmed: false,
                evidence: vec![evidence],
            });
            outcome = LearningOutcome::for_rule(
                LearningStatus::Pending,
                next.last().expect("new observation appended"),
                self.persist,
                true,
            );
        }
        self.commit(next)?;
        Ok(outcome)
    }

    /// Drop a source term's rules, optionally limited to one original/evidence
    /// context. This also removes pending observations, preventing re-promotion.
    pub fn forget(&mut self, wrong: &str, context_id: Option<&str>) -> Result<usize> {
        let mut next = self.rules.clone();
        next.retain(|r| {
            !(r.wrong.eq_ignore_ascii_case(wrong)
                && context_id.is_none_or(|id| {
                    r.context_id == id || r.evidence.iter().any(|e| e.context_id == id)
                }))
        });
        let removed = self.rules.len() - next.len();
        if removed > 0 {
            self.commit(next)?;
        }
        Ok(removed)
    }

    /// Exact matching only, with ASCII token boundaries and no cascading. Screen
    /// text never creates rules; it can only corroborate an already learned one.
    pub fn apply(&self, text: &str, scope: &ContextSnapshot) -> String {
        self.apply_preserving_names(text, scope, &[])
    }

    /// Explicit canonical spellings override older learned aliases, without
    /// deleting or modifying the user's saved rules. Other occurrences still
    /// receive normal corrections (for example Mabe versus lowercase mabe).
    pub fn apply_preserving_names(
        &self,
        text: &str,
        scope: &ContextSnapshot,
        names: &[String],
    ) -> String {
        // Keep the same conservative literal policy as the model refiner. A
        // backtick can start an incomplete span while somebody is editing code;
        // abstain for the complete input instead of guessing its boundaries.
        if text.contains('`') {
            return text.to_owned();
        }
        let spans = crate::postproc::vocab::name_spans(text, names);
        let protected = |start: usize, end: usize| spans.iter().any(|&(s, e)| s < end && e > start);
        let scope = scope.bounded();
        let mut rules: Vec<&LearnedRule> = self
            .rules
            .iter()
            .filter(|r| r.active() && applicable(r, &scope))
            // One spelling can refer to different entities in this utterance.
            // Without per-occurrence evidence, replacing every occurrence can
            // destroy a correct name. Abstain even for confirmed rules; a
            // capitalization-only preference does not merge distinct spellings.
            .filter(|r| {
                r.wrong.eq_ignore_ascii_case(&r.right)
                    || (!contains_term(text, &r.right)
                        && text
                            .char_indices()
                            .filter(|(pos, _)| {
                                term_at(text, *pos, &r.wrong)
                                    && !literal_at(text, *pos, *pos + r.wrong.len())
                                    && !protected(*pos, *pos + r.wrong.len())
                            })
                            .take(2)
                            .count()
                            < 2)
            })
            .collect();
        rules.sort_by_key(|r| std::cmp::Reverse(r.wrong.len()));
        let mut out = String::with_capacity(text.len());
        let mut pos = 0;
        while pos < text.len() {
            let matching: Vec<_> = rules
                .iter()
                .copied()
                .filter(|r| {
                    term_at(text, pos, &r.wrong)
                        && !literal_at(text, pos, pos + r.wrong.len())
                        && !protected(pos, pos + r.wrong.len())
                })
                .collect();
            let hit = matching.first().and_then(|first| {
                let same_source: Vec<_> = matching
                    .iter()
                    .copied()
                    .filter(|r| r.wrong.eq_ignore_ascii_case(&first.wrong))
                    .collect();
                let mut rights = BTreeSet::new();
                for r in &same_source {
                    rights.insert(r.right.as_str());
                }
                if rights.len() == 1 {
                    return Some(*first);
                }
                // More specific confirmed scope wins over an explicit global
                // rule. Otherwise conflicting observations abstain.
                let specific: Vec<_> = same_source
                    .into_iter()
                    .filter(|r| {
                        r.confirmed && !r.context_id.is_empty() && r.context_id == scope.context_id
                    })
                    .collect();
                (specific.len() == 1).then(|| specific[0])
            });
            if let Some(rule) = hit {
                out.push_str(&rule.right);
                pos += rule.wrong.len();
            } else {
                let c = text[pos..]
                    .chars()
                    .next()
                    .expect("valid nonempty text tail");
                out.push(c);
                pos += c.len_utf8();
            }
        }
        out
    }

    /// Candidate terms for a future hotword/model adapter. These are hints, not
    /// independently observed context, and must NEVER be appended to a snapshot
    /// before calling apply(). Unsegmented Chinese screen text is not an entity
    /// recognizer: only an isolated selection is exposed as a Chinese term.
    pub fn candidates(&self, scope: &ContextSnapshot) -> Vec<String> {
        let scope = scope.bounded();
        let mut words = Vec::new();
        let mut seen = BTreeSet::new();
        let mut add = |term: &str| {
            let term = term.trim();
            if words.len() < 64 && valid_term(term) && seen.insert(term.to_ascii_lowercase()) {
                words.push(term.to_owned());
            }
        };
        if scope.selected_text.chars().count() <= MAX_TERM_CHARS {
            add(&scope.selected_text);
        }
        for rule in &self.rules {
            // A generated hint must not smuggle an out-of-context learned name
            // into the model as if it were current independent evidence.
            if rule.active() && applicable(rule, &scope) {
                add(&rule.right);
            }
        }
        for token in scope
            .selected_text
            .split(|c: char| !ascii_word(c))
            .chain(scope.text.split(|c: char| !ascii_word(c)))
        {
            if token.len() >= 2 && token.chars().any(|c| c.is_ascii_alphabetic()) {
                add(token);
            }
        }
        words
    }

    fn commit(&mut self, rules: Vec<LearnedRule>) -> Result<()> {
        if !self.persist {
            self.rules = rules;
            return Ok(());
        }
        let mut encoded = serde_json::to_vec_pretty(&Store {
            version: 1,
            rules: rules.clone(),
        })?;
        encoded.push(b'\n');
        ensure!(
            encoded.len() as u64 <= MAX_STORE_BYTES,
            "terminology memory is too large"
        );
        let parent = self
            .path
            .parent()
            .context("terminology memory has no parent path")?;
        let created = !parent.exists();
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        if created {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp = parent.join(format!(
            ".personalization-{}-{timestamp}-{sequence}.tmp",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| -> Result<()> {
            let mut file = options.open(&tmp)?;
            file.write_all(&encoded)?;
            file.sync_all()?;
            fs::rename(&tmp, &self.path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result.context("save terminology memory")?;
        // Do not mutate in-memory rules unless persistence succeeded.
        self.rules = rules;
        Ok(())
    }
}

fn applicable(rule: &LearnedRule, scope: &ContextSnapshot) -> bool {
    if !rule.program.is_empty() && rule.program != scope.program {
        return false;
    }
    let has_right =
        contains_term(&scope.text, &rule.right) || contains_term(&scope.selected_text, &rule.right);
    let has_wrong =
        contains_term(&scope.text, &rule.wrong) || contains_term(&scope.selected_text, &rule.wrong);
    // Independent context using the source spelling is contradictory evidence,
    // including when both spellings coexist. Even confirmed application/global
    // rules abstain; capitalization-only preferences are not ambiguous here.
    if has_wrong && !rule.wrong.eq_ignore_ascii_case(&rule.right) {
        return false;
    }
    if rule.confirmed && rule.context_id.is_empty() {
        return true;
    }
    let known = !scope.context_id.is_empty()
        && (scope.context_id == rule.context_id
            || rule
                .evidence
                .iter()
                .any(|e| e.context_id == scope.context_id));
    known || has_right
}

fn ascii_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn term_at(text: &str, pos: usize, term: &str) -> bool {
    let end = pos + term.len();
    let Some(part) = text.get(pos..end) else {
        return false;
    };
    if !part.eq_ignore_ascii_case(term) {
        return false;
    }
    let first = term.chars().next().unwrap_or(' ');
    let last = term.chars().last().unwrap_or(' ');
    !(ascii_word(first) && text[..pos].chars().next_back().is_some_and(ascii_word)
        || ascii_word(last) && text[end..].chars().next().is_some_and(ascii_word))
}

fn contains_term(text: &str, term: &str) -> bool {
    !term.is_empty() && text.char_indices().any(|(pos, _)| term_at(text, pos, term))
}

/// Avoid rewriting paths, domain names, shell options and identifiers. Chinese
/// neighbors and ordinary prose punctuation remain legal boundaries. This is
/// intentionally conservative: even explicit global terminology rules are not
/// permission to mutate string literals or source code.
fn literal_at(text: &str, start: usize, end: usize) -> bool {
    let left = text[..start].chars().next_back();
    let right = text[end..].chars().next();
    let code_delimiter = |c: char| "_/@\\=<>+*()[]{}-$".contains(c);
    [left, right].into_iter().flatten().any(code_delimiter)
        || (left == Some('.')
            && text[..start]
                .chars()
                .rev()
                .nth(1)
                .is_none_or(|c| c.is_whitespace() || c.is_ascii_alphanumeric()))
        || (right == Some('.')
            && text[end..]
                .chars()
                .nth(1)
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_'))
        || (left == Some(':') && text[..start].chars().rev().nth(1) == Some(':'))
        || (right == Some(':') && text[end..].chars().nth(1) == Some(':'))
        || (left == right && matches!(left, Some('\'' | '"')))
}

fn bounded_text(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(max)
        .collect()
}

fn valid_term(s: &str) -> bool {
    (2..=MAX_TERM_CHARS).contains(&s.chars().count())
        && s.trim() == s
        && !s.chars().any(|c| {
            c.is_control()
                || matches!(
                    c,
                    '.' | '。' | '!' | '！' | '?' | '？' | ',' | '，' | ';' | '；'
                )
        })
        && s.split_whitespace().count() <= 4
        && s.chars().any(char::is_alphabetic)
}

fn validate_pair(wrong: &str, right: &str) -> Result<()> {
    ensure!(
        valid_term(wrong) && valid_term(right),
        "corrections must be short terms, not sentences"
    );
    ensure!(wrong != right, "correction must change the term");
    Ok(())
}

/// Extract one compact substitution, expanding ASCII edits to complete words.
/// A Chinese one-character substitution requires either a complete selected
/// target term or standalone short names; never store a one-character rule.
fn compact_edit(before: &str, after: &str, selected: &str) -> Option<(String, String)> {
    let a: Vec<char> = before.chars().collect();
    let b: Vec<char> = after.chars().collect();
    if a.len() > 2048 || b.len() > 2048 || a == b {
        return None;
    }
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (mut start, mut end_a, mut end_b) = (prefix, a.len() - suffix, b.len() - suffix);
    // Pure insertions/deletions are edits of meaning or unfinished typing, not
    // stable evidence that the recognizer substitutes one term for another.
    if start == end_a || start == end_b {
        // An internal spelling insertion (mabe -> maybe) is a complete token
        // correction. Adding/removing surrounding words or truncating an
        // unfinished token is not.
        let internal_ascii = start > 0
            && end_a < a.len()
            && end_b < b.len()
            && ascii_word(a[start - 1])
            && ascii_word(b[start - 1])
            && ascii_word(a[end_a])
            && ascii_word(b[end_b]);
        if !internal_ascii {
            return None;
        }
    }
    if selected != after && valid_term(selected) && selected.chars().count() <= 16 {
        let selected_chars: Vec<char> = selected.chars().collect();
        let occurrences: Vec<usize> = b
            .windows(selected_chars.len())
            .enumerate()
            .filter_map(|(i, w)| (w == selected_chars).then_some(i))
            .collect();
        if occurrences.len() == 1 {
            let left = occurrences[0];
            let right = left + selected_chars.len();
            if left <= start && right >= end_b {
                end_a += right - end_b;
                end_b = right;
                start = left;
            }
        }
    }
    if a[start..end_a]
        .iter()
        .chain(&b[start..end_b])
        .any(|c| c.is_ascii_alphabetic())
    {
        while start > 0 && ascii_word(a[start - 1]) && ascii_word(b[start - 1]) {
            start -= 1;
        }
        while end_a < a.len() && ascii_word(a[end_a]) {
            end_a += 1;
        }
        while end_b < b.len() && ascii_word(b[end_b]) {
            end_b += 1;
        }
    } else if (2..=4).contains(&a.len())
        && (2..=4).contains(&b.len())
        && a[0] == b[0]
        && common_surname(a[0])
        && a.iter()
            .chain(&b)
            .all(|c| ('\u{3400}'..='\u{9fff}').contains(c))
    {
        start = 0;
        end_a = a.len();
        end_b = b.len();
    }
    let wrong: String = a[start..end_a].iter().collect();
    let right: String = b[start..end_b].iter().collect();
    if validate_pair(&wrong, &right).is_err() {
        return None;
    }
    // Observed English replacements must be one token, not a phrase rewrite.
    if wrong.chars().any(char::is_whitespace) || right.chars().any(char::is_whitespace) {
        return None;
    }
    // Reject mostly different terms and whole-sentence substitutions. Short
    // isolated corrections (mabe/maybe, Chinese names) remain useful evidence.
    let max_len = wrong.chars().count().max(right.chars().count());
    if max_len > 24 || edit_distance(&wrong, &right) > max_len.div_ceil(2) {
        return None;
    }
    if start == 0 && end_a == a.len() && end_b == b.len() && max_len > 8 {
        return None;
    }
    Some((wrong, right))
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ac) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, bc) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1)
                .min(row[j] + 1)
                .min(diagonal + usize::from(!ac.eq_ignore_ascii_case(bc)));
            diagonal = above;
        }
    }
    row[b.len()]
}

fn correction_pair(before: &str, after: &str, selected: &str) -> Option<(String, String)> {
    compact_honorific_name(before, after)
        .or_else(|| {
            compact_edit(before, after, selected).filter(|(wrong, right)| {
                // A span crossing an honorific can accidentally merge edits to two
                // different people. A real name correction is extracted above.
                !["教授", "老師", "博士", "先生", "小姐", "女士"]
                    .iter()
                    .any(|title| wrong.contains(title) || right.contains(title))
            })
        })
        .or_else(|| {
            // Ignore only CJK/ASCII boundary spacing in a comparison copy, never
            // English word boundaries, punctuation, casing or the delivered text.
            // The fallback can recover one ASCII spelling pair, not phrase edits.
            if before.contains('`') || after.contains('`') {
                return None;
            }
            let a = diff_boundary_spacing(before);
            let b = diff_boundary_spacing(after);
            if a == before && b == after {
                return None;
            }
            compact_edit(&a, &b, "").filter(|(wrong, right)| {
                wrong.bytes().all(|c| c.is_ascii_alphabetic())
                    && right.bytes().all(|c| c.is_ascii_alphabetic())
            })
        })
        .filter(|(wrong, right)| {
            // Apply attribution to strict and spacing-recovery paths alike. Do not
            // borrow an unchanged occurrence elsewhere to justify an edit in code.
            if !unique_plain_term(before, wrong) || !unique_plain_term(after, right) {
                return false;
            }
            // Rearranging the same Han characters can be a wording edit, not an
            // acoustic spelling substitution. This deliberately also abstains on
            // transposed name characters; explicit learn() remains available.
            let mut a: Vec<_> = wrong.chars().collect();
            let mut b: Vec<_> = right.chars().collect();
            if a.iter()
                .chain(&b)
                .all(|c| ('\u{3400}'..='\u{9fff}').contains(c))
            {
                a.sort_unstable();
                b.sort_unstable();
                a != b
            } else {
                true
            }
        })
}

fn unique_plain_term(text: &str, term: &str) -> bool {
    if text.contains('`') || term.chars().any(|c| "_/@\\=<>+*()[]{}-$\"'".contains(c)) {
        return false;
    }
    let mut occurrences = text
        .char_indices()
        .filter(|(pos, _)| term_at(text, *pos, term));
    let Some((pos, _)) = occurrences.next() else {
        return false;
    };
    occurrences.next().is_none() && !literal_at(text, pos, pos + term.len())
}

fn diff_boundary_spacing(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while pos < chars.len() {
        if chars[pos] != ' ' {
            out.push(chars[pos]);
            pos += 1;
            continue;
        }
        let start = pos;
        while pos < chars.len() && chars[pos] == ' ' {
            pos += 1;
        }
        let han = |c: char| ('\u{3400}'..='\u{9fff}').contains(&c);
        let boundary = start
            .checked_sub(1)
            .and_then(|i| chars.get(i))
            .zip(chars.get(pos))
            .is_some_and(|(&left, &right)| {
                (han(left) && ascii_word(right)) || (ascii_word(left) && han(right))
            });
        if !boundary {
            out.extend(chars[start..pos].iter());
        }
    }
    out
}

/// A finalized corrected sentence may contain a one-character name edit.
/// Honorifics provide a useful boundary, unlike arbitrary unsegmented Chinese.
/// Require an unchanged common surname and exactly one possible 2–4 character
/// name. This deliberately abstains on ambiguous boundaries/compound surnames.
fn compact_honorific_name(before: &str, after: &str) -> Option<(String, String)> {
    let a: Vec<char> = before.chars().collect();
    let b: Vec<char> = after.chars().collect();
    if a.len() != b.len() || a.len() > 2048 || a == b {
        return None;
    }
    let changed: Vec<usize> = a
        .iter()
        .zip(&b)
        .enumerate()
        .filter_map(|(i, (x, y))| (x != y).then_some(i))
        .collect();
    if changed.len() > 2 {
        return None;
    }
    let first = changed[0];
    let last = *changed.last()?;
    if last - first > 1 {
        return None;
    }
    let mut pairs = Vec::new();
    for end in (last + 1)..=(last + 2).min(b.len()) {
        let tail: String = b[end..].iter().collect();
        if !["教授", "老師", "博士", "先生", "小姐", "女士"]
            .iter()
            .any(|s| tail.starts_with(s))
        {
            continue;
        }
        for length in 2..=4 {
            if end < length {
                continue;
            }
            let start = end - length;
            if start >= first || !common_surname(b[start]) || a[start] != b[start] {
                continue;
            }
            if !a[start..end]
                .iter()
                .chain(&b[start..end])
                .all(|c| ('\u{3400}'..='\u{9fff}').contains(c))
            {
                continue;
            }
            let wrong: String = a[start..end].iter().collect();
            let right: String = b[start..end].iter().collect();
            pairs.push((wrong, right));
        }
    }
    if pairs.len() == 1 {
        pairs.pop().filter(|(wrong, right)| {
            // If only one of two identical names was edited, do not assume the
            // untouched one refers to the same person and learn a blanket fix.
            before.matches(wrong.as_str()).count() == 1
                && after.matches(right.as_str()).count() == 1
        })
    } else {
        None
    }
}

fn common_surname(c: char) -> bool {
    "陳林黃張李王吳劉蔡楊許鄭謝郭洪曾邱廖賴徐周葉蘇莊呂江何蕭羅高潘簡朱鍾游彭詹胡施沈余盧梁趙顏柯翁魏孫戴范方宋鄧杜侯曹薛丁卓阮馬董温溫唐藍石蔣古紀姚連馮歐程湯傅康田汪白鄒巫尤鐘嚴龔".contains(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        dir: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            Self {
                dir: std::env::temp_dir().join(format!(
                    "voicetype-memory-test-{}-{sequence}",
                    std::process::id()
                )),
            }
        }
        fn memory(&self) -> Personalization {
            Personalization::load(&self.dir.join("memory.json")).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
    fn scope(id: &str, text: &str) -> ContextSnapshot {
        ContextSnapshot {
            program: "browser".into(),
            context_id: id.into(),
            text: text.into(),
            ..Default::default()
        }
    }

    #[test]
    fn explicit_user_examples_match_whole_english_tokens_with_chinese_neighbors() {
        let f = Fixture::new();
        let mut m = f.memory();
        let s = ContextSnapshot::default();
        m.learn("mabe", "maybe", &s).unwrap();
        m.learn("gthub", "GitHub", &s).unwrap();
        assert_eq!(
            m.apply("這個東西mabe是對的，push到gthub上面", &s),
            "這個東西maybe是對的，push到GitHub上面"
        );
        assert_eq!(
            m.apply("mabelline gthubber _gthub gthub_ GTHUB", &s),
            "mabelline gthubber _gthub gthub_ GitHub"
        );
    }

    #[test]
    fn observations_require_independent_sessions_and_do_not_become_global() {
        let f = Fixture::new();
        let mut m = f.memory();
        let s = scope("conversation-a", "");
        assert!(m
            .observe_correction("用gthub上傳", "用GitHub上傳", &s, 1)
            .unwrap());
        assert!(!m
            .observe_correction("用gthub上傳", "用GitHub上傳", &s, 1)
            .unwrap());
        assert_eq!(m.apply("推到gthub", &s), "推到gthub");
        m.observe_correction("用gthub上傳", "用GitHub上傳", &s, 2)
            .unwrap();
        assert_eq!(m.apply("推到gthub", &s), "推到GitHub");
        assert_eq!(
            m.apply("推到gthub", &scope("other", "GitHubber")),
            "推到gthub"
        );
        assert_eq!(
            m.apply("推到gthub", &scope("other", "GitHub 專案")),
            "推到GitHub"
        );
        assert_eq!(
            m.apply("推到gthub", &scope("other", "gthub 和 GitHub 都在這裡")),
            "推到gthub"
        );
    }

    #[test]
    fn terms_survive_restart_without_persisting_surrounding_text() {
        let f = Fixture::new();
        let s = scope("window-1", "This surrounding text must stay ephemeral");
        {
            let mut m = f.memory();
            m.observe_correction("mabe很好", "maybe很好", &s, 1)
                .unwrap();
            m.observe_correction("mabe很好", "maybe很好", &s, 2)
                .unwrap();
        }
        let mut m = f.memory();
        assert_eq!(
            m.apply("mabe很好", &scope("new-window", "maybe")),
            "maybe很好"
        );
        let data = fs::read_to_string(&m.path).unwrap();
        assert!(!data.contains("surrounding text"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&m.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(m.forget("mabe", None).unwrap(), 1);
        assert!(f.memory().list().is_empty());
    }

    #[test]
    fn professor_names_need_confirmed_mapping_not_similarity_to_screen_text() {
        let f = Fixture::new();
        let mut m = f.memory();
        let s = scope("professors", "陳柏宇教授");
        assert_eq!(m.apply("問陳博宇教授", &s), "問陳博宇教授");
        m.learn("陳博宇", "陳柏宇", &s).unwrap();
        assert_eq!(m.apply("問陳博宇教授", &s), "問陳柏宇教授");
        assert_eq!(
            m.apply("問陳博宇教授", &scope("different", "別的老師")),
            "問陳博宇教授"
        );
        assert_eq!(
            m.apply("問陳博宇教授", &scope("new", "陳柏宇教授")),
            "問陳柏宇教授"
        );
        assert_eq!(
            m.apply("問陳博宇教授", &scope("new", "陳博宇與陳柏宇")),
            "問陳博宇教授"
        );
    }

    #[test]
    fn single_character_name_edits_need_a_complete_selected_target() {
        assert!(compact_edit("問陳博宇教授的研究方向", "問陳柏宇教授的研究方向", "").is_none());
        assert_eq!(
            compact_edit("問陳博宇教授的研究方向", "問陳柏宇教授的研究方向", "陳柏宇"),
            Some(("陳博宇".into(), "陳柏宇".into()))
        );
        assert_eq!(
            compact_edit("陳博宇", "陳柏宇", ""),
            Some(("陳博宇".into(), "陳柏宇".into()))
        );
    }

    #[test]
    fn ignores_additions_deletions_rewrites_and_intermediate_typing() {
        let f = Fixture::new();
        let mut m = f.memory();
        let s = scope("typing", "");
        for (before, after) in [
            ("hello", "hello world"),
            ("hello world", "hello"),
            ("我覺得這個方法很好", "算了還是換另一個想法"),
            ("mabe", "m"),
            ("", "GitHub"),
        ] {
            assert!(
                !m.observe_correction(before, after, &s, 1).unwrap(),
                "{before:?} -> {after:?}"
            );
        }
        assert!(m.list().is_empty());
        m.observe_correction("catch", "cache", &s, 2).unwrap();
        assert_eq!(m.apply("try catch", &s), "try catch");
        m.observe_correction("catch", "cache", &s, 3).unwrap();
        assert_eq!(
            m.apply("try catch", &scope("code", "try catch cache")),
            "try catch"
        );
        assert_eq!(
            m.apply("try catch", &ContextSnapshot::default()),
            "try catch"
        );
    }

    #[test]
    fn conflicting_observations_abstain_and_explicit_confirmation_resolves() {
        let f = Fixture::new();
        let mut m = f.memory();
        let s = scope("professors", "");
        for (session, right) in [(1, "陳柏宇"), (2, "陳柏宇"), (3, "陳伯宇"), (4, "陳伯宇")]
        {
            m.observe_correction("陳博宇", right, &s, session).unwrap();
        }
        assert_eq!(m.apply("找陳博宇教授", &s), "找陳博宇教授");
        m.learn("陳博宇", "陳柏宇", &s).unwrap();
        assert_eq!(m.apply("找陳博宇教授", &s), "找陳柏宇教授");
    }

    #[test]
    fn context_and_candidates_are_bounded_and_do_not_trigger_rules() {
        let f = Fixture::new();
        let mut m = f.memory();
        let mut s = scope("one", "GitHub GitHubber 中英文 maybe push");
        s.selected_text = "陳柏宇".into();
        assert!(m.candidates(&s).contains(&"陳柏宇".to_owned()));
        assert!(m.candidates(&s).contains(&"GitHub".to_owned()));
        m.learn("mabe", "maybe", &s).unwrap();
        let other = scope("other", "");
        assert!(!m.candidates(&other).contains(&"maybe".to_owned()));
        assert_eq!(m.apply("mabe", &other), "mabe");
        s.text = "中".repeat(5000);
        assert_eq!(s.bounded().text.chars().count(), 4096);
    }

    #[test]
    fn failed_write_does_not_change_live_rules() {
        let f = Fixture::new();
        let mut m = f.memory();
        fs::create_dir_all(&f.dir).unwrap();
        fs::create_dir(&m.path).unwrap();
        assert!(m
            .learn("mabe", "maybe", &ContextSnapshot::default())
            .is_err());
        assert!(m.list().is_empty());
    }

    #[test]
    fn explicit_full_sentence_can_confirm_a_bounded_professor_name() {
        let mut m = Personalization::memory();
        let mut s = scope("conversation", "");
        s.selected_text = "我想問陳柏宇教授的研究方向".into();
        assert!(m
            .confirm_correction("我想問陳博宇教授的研究方向", &s.selected_text, &s)
            .unwrap());
        assert_eq!(m.list()[0].wrong, "陳博宇");
        assert_eq!(m.list()[0].right, "陳柏宇");
        assert_eq!(m.apply("陳博宇教授的研究", &s), "陳柏宇教授的研究");
        assert!(compact_honorific_name("陳博宇和李明宇教授", "陳柏宇和李明宇教授").is_none());
        assert!(compact_edit("這個東西很好", "這個東西很棒", "").is_none());
    }

    #[test]
    fn ephemeral_fallback_can_learn_without_writing_disk() {
        let mut m = Personalization::memory();
        m.learn("mabe", "maybe", &ContextSnapshot::default())
            .unwrap();
        assert_eq!(m.apply("mabe", &ContextSnapshot::default()), "maybe");
        assert!(!m.persist);
        assert!(m.path.as_os_str().is_empty());
    }

    #[test]
    fn replacement_does_not_cascade_into_a_second_rule() {
        let mut m = Personalization::memory();
        let s = ContextSnapshot::default();
        m.learn("mabe", "maybe", &s).unwrap();
        m.learn("maybe", "possibly", &s).unwrap();
        assert_eq!(m.apply("mabe", &s), "maybe");
    }

    #[test]
    fn learned_spelling_never_rewrites_literals_paths_or_identifiers() {
        let mut m = Personalization::memory();
        let s = ContextSnapshot::default();
        m.learn("mabe", "maybe", &s).unwrap();
        m.learn("gthub", "GitHub", &s).unwrap();
        for literal in [
            "保留`mabe`，還有mabe",
            "未閉合的`mabe",
            "```\ngthub\n```",
            "/srv/gthub/config.json",
            "C:\\gthub\\config.json",
            "gthub/config",
            "https://gthub.com",
            "gthub.json",
            "api.gthub",
            ".gthub",
            "放在 .gthub",
            "my_mabe",
            "mabe_value",
            "Mabel",
            "mabelline",
            "my-mabe",
            "--mabe",
            "mabe()",
            "$mabe",
            "mabe=true",
            "name=mabe",
            "module::mabe",
            "mabe::method",
            "\"mabe\"",
            "'gthub'",
        ] {
            assert_eq!(m.apply(literal, &s), literal, "literal changed: {literal}");
        }
        assert_eq!(
            m.apply("這個mabe是對的，push到gthub上面", &s),
            "這個maybe是對的，push到GitHub上面"
        );
        assert_eq!(
            m.apply("mabe. gthub, mabe: 對的", &s),
            // Repeated source spellings now abstain without occurrence-level
            // evidence, while the single unrelated term can still be corrected.
            "mabe. GitHub, mabe: 對的"
        );
        assert_eq!(
            m.apply("路徑/srv/gthub，但gthub是平台", &s),
            "路徑/srv/gthub，但GitHub是平台"
        );
    }

    #[test]
    fn learned_names_only_become_model_candidates_in_corroborated_contexts() {
        let mut m = Personalization::memory();
        let source = scope("professor-a", "");
        m.observe_correction("陳博宇", "陳柏宇", &source, 1)
            .unwrap();
        assert!(!m.candidates(&source).contains(&"陳柏宇".into()));
        m.observe_correction("陳博宇", "陳柏宇", &source, 2)
            .unwrap();
        assert!(m.candidates(&source).contains(&"陳柏宇".into()));
        assert!(!m.candidates(&scope("other", "")).contains(&"陳柏宇".into()));
        assert!(m
            .candidates(&scope("other", "陳柏宇教授"))
            .contains(&"陳柏宇".into()));
        assert!(!m
            .candidates(&scope("other", "陳博宇和陳柏宇教授"))
            .contains(&"陳柏宇".into()));
    }

    #[test]
    fn repeated_full_professor_sentence_corrections_learn_only_the_name() {
        let mut m = Personalization::memory();
        let s = scope("graduate-applications", "");
        let before = "我想問陳博宇教授的研究方向";
        let after = "我想問陳柏宇教授的研究方向";
        assert!(m.observe_correction(before, after, &s, 1).unwrap());
        assert_eq!(m.list()[0].wrong, "陳博宇");
        assert_eq!(m.list()[0].right, "陳柏宇");
        assert!(!m.list()[0].active());
        assert_eq!(m.apply(before, &s), before);
        assert!(!m.observe_correction(before, after, &s, 1).unwrap());
        assert!(m.observe_correction(before, after, &s, 2).unwrap());
        assert_eq!(m.apply(before, &s), after);
        assert_eq!(m.apply("博士說博宇這個名字", &s), "博士說博宇這個名字");
        assert_eq!(m.apply(before, &scope("unrelated", "")), before);
    }

    #[test]
    fn observed_name_edits_abstain_on_ambiguous_or_multiple_people() {
        let mut m = Personalization::memory();
        let s = scope("graduate-applications", "");
        for (before, after) in [
            ("我想問黃陳博宇教授的研究", "我想問黃陳柏宇教授的研究"),
            ("陳博宇教授與李明宇教授", "陳柏宇教授與李名宇教授"),
            ("陳博宇教授與陳博宇教授", "陳柏宇教授與陳博宇教授"),
            ("陳博宇和李明宇教授", "陳柏宇和李明宇教授"),
            ("我想問陳博宇教授的研究", "我想問林柏宇教授的研究"),
        ] {
            for session in [1, 2] {
                assert!(
                    !m.observe_correction(before, after, &s, session).unwrap(),
                    "unsafe name pair accepted: {before} -> {after}"
                );
            }
        }
        assert!(m.list().is_empty());
    }
}
