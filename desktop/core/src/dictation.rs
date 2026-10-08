//! Provider ownership and exactly-once delivery, independent of OS audio/input.
use crate::{AppError, Provider};
use serde::Serialize;
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

/// The OS monotonic clock boundary, substitutable without changing deadlines.
pub trait SessionClock: Send + Sync {
    fn now(&self) -> Instant;
}

struct SystemClock;
impl SessionClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionKey {
    provider: Provider,
    id: u64,
}

impl SessionKey {
    pub fn provider(self) -> Provider {
        self.provider
    }
    pub fn id(self) -> u64 {
        self.id
    }
}

/// An OS-owned editable input context and its focus generation. Window identity
/// alone is insufficient: a different field in the same window is a new lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetLease {
    pub context_id: String,
    pub generation: u64,
}

impl TargetLease {
    pub fn new(context_id: impl Into<String>, generation: u64) -> Self {
        Self {
            context_id: context_id.into(),
            generation,
        }
    }
}

/// Bounded, transient text from the original input context, never diagnostics.
#[derive(Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub struct DictationContext {
    pub program: String,
    pub context_text: String,
    pub selected_text: String,
}

impl std::fmt::Debug for DictationContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DictationContext").finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderCommand {
    Start {
        key: SessionKey,
        target: TargetLease,
        context: DictationContext,
    },
    Stop {
        key: SessionKey,
    },
    Cancel {
        key: SessionKey,
    },
}

/// Boundary to the external engine process. An error means the command was not
/// accepted. A partial/uncertain write must instead retain ownership and produce
/// failure + release events after the adapter has proved cleanup.
pub trait ProviderPort {
    fn send(&mut self, command: ProviderCommand) -> Result<(), CommandRejected>;
}

#[derive(Debug, thiserror::Error)]
#[error("provider did not accept the command")]
pub struct CommandRejected;

pub enum ProviderEvent {
    Recording,
    Final(String),
    Failed(SessionFailure),
    /// Only after capture and child work are quiescent. A result alone is not a
    /// release acknowledgement; adapters must not synthesize this prematurely.
    Released,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionFailure {
    TimedOut,
    InvalidResult,
    ProviderFailed,
}

#[derive(Debug, Serialize)]
pub struct DictationStatus {
    pub busy: bool,
    pub failure: Option<SessionFailure>,
    pub has_retained_text: bool,
    pub phase: Option<DictationPhase>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryOutcome {
    Delivered,
    FocusChanged,
    Partial,
    Unconfirmed,
}

/// Last undelivered result for the recovery UI. Deliberately not Debug or part of
/// diagnostic snapshots: transcripts must not end up in status logs.
pub struct RetainedText {
    pub key: SessionKey,
    pub text: String,
    pub reason: DeliveryOutcome,
}

pub trait DeliveryPort {
    /// Validate the entire lease and commit as one OS input-context operation.
    /// Implementations must never force focus, retry partial insertion, or treat
    /// a separate foreground-window check followed by SendInput as atomic.
    fn commit_if_focused(&mut self, target: &TargetLease, text: &str) -> DeliveryOutcome;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DictationPhase {
    Preparing,
    Recording,
    Finalizing,
    Releasing,
}

use DictationPhase as Phase;

struct Active {
    key: SessionKey,
    target: TargetLease,
    phase: Phase,
    cancel_sent: bool,
    since: Instant,
}

pub(super) struct Coordinator {
    active: Option<Active>,
    retained: Option<RetainedText>,
    failure: Option<SessionFailure>,
    clock: Arc<dyn SessionClock>,
}

impl Default for Coordinator {
    fn default() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }
}

impl Coordinator {
    pub fn with_clock(clock: Arc<dyn SessionClock>) -> Self {
        Self {
            active: None,
            retained: None,
            failure: None,
            clock,
        }
    }

    /// Timeout invalidates output and requests cancellation. The adapter must
    /// still prove child/capture cleanup before supplying Released.
    pub fn expire(&mut self, port: &mut impl ProviderPort) -> bool {
        let Some(active) = &self.active else {
            return false;
        };
        let limit = match active.phase {
            Phase::Preparing => Duration::from_secs(6),
            Phase::Recording => Duration::from_secs(65),
            Phase::Finalizing => Duration::from_secs(120),
            Phase::Releasing => Duration::from_secs(3),
        };
        if self.clock.now().saturating_duration_since(active.since) < limit {
            return false;
        }
        self.failure.get_or_insert(SessionFailure::TimedOut);
        let _ = self.cancel(port);
        true
    }
    pub fn busy(&self) -> bool {
        self.active.is_some()
    }
    pub fn retained_text(&self) -> Option<&RetainedText> {
        self.retained.as_ref()
    }
    pub fn dismiss_retained(&mut self, provider: Provider, session: u64) -> bool {
        if self
            .retained
            .as_ref()
            .is_some_and(|text| text.key.provider == provider && text.key.id == session)
        {
            self.retained = None;
            true
        } else {
            false
        }
    }
    pub fn status(&self) -> DictationStatus {
        DictationStatus {
            busy: self.busy(),
            failure: self.failure,
            has_retained_text: self.retained.is_some(),
            phase: self.active.as_ref().map(|active| active.phase),
        }
    }

    pub fn start(
        &mut self,
        provider: Provider,
        target: TargetLease,
        context: DictationContext,
        port: &mut impl ProviderPort,
    ) -> Result<SessionKey, AppError> {
        if self.busy() {
            return Err(AppError::DictationBusy);
        }
        let id = NEXT_SESSION
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| AppError::SessionIdsExhausted)?;
        let key = SessionKey { provider, id };
        port.send(ProviderCommand::Start {
            key,
            target: target.clone(),
            context,
        })
        .map_err(|_| AppError::ProviderUnavailable)?;
        self.active = Some(Active {
            key,
            target,
            phase: Phase::Preparing,
            cancel_sent: false,
            since: self.clock.now(),
        });
        self.failure = None;
        Ok(key)
    }

    pub fn stop(&mut self, port: &mut impl ProviderPort) -> Result<(), AppError> {
        let Some(active) = self.active.as_mut() else {
            return Ok(());
        };
        if active.phase == Phase::Preparing {
            return self.cancel(port);
        }
        if active.phase == Phase::Recording {
            port.send(ProviderCommand::Stop { key: active.key })
                .map_err(|_| AppError::ProviderUnavailable)?;
            active.phase = Phase::Finalizing;
            active.since = self.clock.now();
        }
        Ok(())
    }

    pub fn cancel(&mut self, port: &mut impl ProviderPort) -> Result<(), AppError> {
        let Some(active) = self.active.as_mut() else {
            return Ok(());
        };
        // Invalidate results before IO, even if cancellation cannot be sent yet.
        if active.phase != Phase::Releasing {
            active.phase = Phase::Releasing;
            active.since = self.clock.now();
        }
        if !active.cancel_sent {
            port.send(ProviderCommand::Cancel { key: active.key })
                .map_err(|_| AppError::ProviderUnavailable)?;
            active.cancel_sent = true;
        }
        Ok(())
    }

    pub fn event(&mut self, key: SessionKey, event: ProviderEvent, output: &mut impl DeliveryPort) {
        let Some(active) = self.active.as_mut().filter(|a| a.key == key) else {
            return;
        };
        match event {
            ProviderEvent::Recording if active.phase == Phase::Preparing => {
                active.phase = Phase::Recording;
                active.since = self.clock.now();
            }
            ProviderEvent::Final(text) if active.phase == Phase::Finalizing => {
                active.phase = Phase::Releasing;
                active.since = self.clock.now();
                if text.len() > 64 * 1024 || text.trim().is_empty() || text.contains('\0') {
                    self.failure = Some(SessionFailure::InvalidResult);
                    return;
                }
                let reason = output.commit_if_focused(&active.target, &text);
                if reason != DeliveryOutcome::Delivered {
                    self.retained = Some(RetainedText { key, text, reason });
                }
            }
            ProviderEvent::Released => {
                if active.phase != Phase::Releasing {
                    self.failure.get_or_insert(SessionFailure::ProviderFailed);
                }
                self.active = None;
            }
            ProviderEvent::Failed(reason) => {
                if active.phase != Phase::Releasing {
                    active.phase = Phase::Releasing;
                    active.since = self.clock.now();
                }
                self.failure.get_or_insert(reason);
            }
            _ => {}
        }
    }
}
