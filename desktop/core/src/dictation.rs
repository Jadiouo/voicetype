//! Provider ownership and exactly-once delivery, independent of OS audio/input.
use crate::{AppError, Provider};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderCommand {
    Start {
        key: SessionKey,
        target: TargetLease,
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
}

#[derive(Debug, Serialize)]
pub struct DictationStatus {
    pub busy: bool,
    pub failure: Option<SessionFailure>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Delivered,
    FocusChanged,
    Partial,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Preparing,
    Recording,
    Finalizing,
    Releasing,
}

struct Active {
    key: SessionKey,
    target: TargetLease,
    phase: Phase,
    cancel_sent: bool,
}

#[derive(Default)]
pub(super) struct Coordinator {
    active: Option<Active>,
    retained: Option<RetainedText>,
    failure: Option<SessionFailure>,
}

impl Coordinator {
    pub fn busy(&self) -> bool {
        self.active.is_some()
    }
    pub fn retained_text(&self) -> Option<&RetainedText> {
        self.retained.as_ref()
    }
    pub fn status(&self) -> DictationStatus {
        DictationStatus {
            busy: self.busy(),
            failure: self.failure,
        }
    }

    pub fn start(
        &mut self,
        provider: Provider,
        target: TargetLease,
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
        })
        .map_err(|_| AppError::ProviderUnavailable)?;
        self.active = Some(Active {
            key,
            target,
            phase: Phase::Preparing,
            cancel_sent: false,
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
        }
        Ok(())
    }

    pub fn cancel(&mut self, port: &mut impl ProviderPort) -> Result<(), AppError> {
        let Some(active) = self.active.as_mut() else {
            return Ok(());
        };
        // Invalidate results before IO, even if cancellation cannot be sent yet.
        active.phase = Phase::Releasing;
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
                active.phase = Phase::Recording
            }
            ProviderEvent::Final(text) if active.phase == Phase::Finalizing => {
                active.phase = Phase::Releasing;
                if text.len() > 64 * 1024 || text.trim().is_empty() || text.contains('\0') {
                    self.failure = Some(SessionFailure::InvalidResult);
                    return;
                }
                let reason = output.commit_if_focused(&active.target, &text);
                if reason != DeliveryOutcome::Delivered {
                    self.retained = Some(RetainedText { key, text, reason });
                }
            }
            ProviderEvent::Released => self.active = None,
            ProviderEvent::Failed(reason) => {
                active.phase = Phase::Releasing;
                self.failure = Some(reason);
            }
            _ => {}
        }
    }
}
