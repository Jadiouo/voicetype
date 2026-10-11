//! Ctrl+CapsLock toggle state, independent of Win32 message delivery.
use crate::TargetLease;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GestureAction {
    Start(TargetLease),
    Stop,
    Cancel,
}

pub(crate) struct KeyOutcome {
    pub action: Option<GestureAction>,
    pub suppress: bool,
}

#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    PendingStart,
    Listening,
    Finalizing,
}

#[derive(Default)]
pub(crate) struct WindowsGesture {
    phase: Phase,
    caps_latched: bool,
    escape_latched: bool,
    escape_consumed: bool,
    owner_active: bool,
}

impl WindowsGesture {
    #[cfg(windows)]
    pub fn needs_target(&self) -> bool {
        matches!(self.phase, Phase::Idle) && !self.caps_latched
    }

    pub fn caps(
        &mut self,
        down: bool,
        ctrl: bool,
        target: impl FnOnce() -> Option<TargetLease>,
    ) -> KeyOutcome {
        let suppress = self.caps_latched || ctrl;
        if !down {
            self.caps_latched = false;
            return KeyOutcome {
                action: None,
                suppress,
            };
        }
        if !ctrl || self.caps_latched {
            return KeyOutcome {
                action: None,
                suppress,
            };
        }
        self.caps_latched = true;
        let action = match self.phase {
            Phase::Idle => target().map(|target| {
                self.phase = Phase::PendingStart;
                GestureAction::Start(target)
            }),
            Phase::PendingStart | Phase::Listening => {
                self.phase = Phase::Finalizing;
                Some(GestureAction::Stop)
            }
            Phase::Finalizing => None,
        };
        KeyOutcome { action, suppress }
    }
    pub fn escape(&mut self, down: bool) -> KeyOutcome {
        if !down {
            let suppress = self.escape_consumed;
            self.escape_latched = false;
            self.escape_consumed = false;
            return KeyOutcome {
                action: None,
                suppress,
            };
        }
        if self.escape_latched {
            return KeyOutcome {
                action: None,
                suppress: self.escape_consumed,
            };
        }
        self.escape_latched = true;
        if matches!(self.phase, Phase::Idle) {
            return KeyOutcome {
                action: None,
                suppress: false,
            };
        }
        self.phase = Phase::Finalizing;
        self.escape_consumed = true;
        KeyOutcome {
            action: Some(GestureAction::Cancel),
            suppress: true,
        }
    }
    pub fn observe_owner(&mut self, active: bool) {
        if active {
            self.owner_active = true;
            if matches!(self.phase, Phase::PendingStart) {
                self.phase = Phase::Listening;
            }
        } else if self.owner_active {
            self.owner_active = false;
            self.phase = Phase::Idle;
        }
    }
    pub fn reject_start(&mut self) {
        if !self.owner_active {
            self.phase = Phase::Idle;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(id: u64) -> TargetLease {
        TargetLease::new("win:edit:1", id)
    }

    #[test]
    fn pending_start_survives_old_idle_poll_then_stops_once_and_restarts_after_release() {
        let mut gesture = WindowsGesture::default();
        assert_eq!(
            gesture.caps(true, true, || Some(lease(1))).action,
            Some(GestureAction::Start(lease(1)))
        );
        assert!(
            gesture.caps(true, true, || Some(lease(2))).action.is_none(),
            "repeat must not restart"
        );
        assert!(
            gesture.caps(false, true, || None).suppress,
            "chord key-up remains consumed"
        );
        gesture.observe_owner(false); // worker's old idle state before it reads Start
        gesture.observe_owner(true); // Start accepted
        assert_eq!(
            gesture.caps(true, true, || None).action,
            Some(GestureAction::Stop)
        );
        gesture.caps(false, true, || None);
        gesture.observe_owner(true); // Finalizing must not turn Stop back into listening
        assert!(gesture.caps(true, true, || Some(lease(2))).action.is_none());
        gesture.caps(false, true, || None);
        gesture.observe_owner(false); // actual Released
        assert_eq!(
            gesture.caps(true, true, || Some(lease(2))).action,
            Some(GestureAction::Start(lease(2)))
        );
    }

    #[test]
    fn rejected_start_and_cancelled_pending_start_recover() {
        let mut gesture = WindowsGesture::default();
        assert_eq!(
            gesture.caps(true, true, || Some(lease(1))).action,
            Some(GestureAction::Start(lease(1)))
        );
        gesture.reject_start();
        gesture.caps(false, true, || None);
        assert_eq!(
            gesture.caps(true, true, || Some(lease(2))).action,
            Some(GestureAction::Start(lease(2)))
        );
        assert_eq!(gesture.escape(true).action, Some(GestureAction::Cancel));
        assert!(
            gesture.escape(true).action.is_none(),
            "Esc repeat must not requeue cancel"
        );
        assert!(
            gesture.escape(false).suppress,
            "consumed Esc down requires a consumed key-up"
        );
        gesture.observe_owner(true);
        gesture.observe_owner(false);
        gesture.caps(false, false, || None);
        assert_eq!(
            gesture.caps(true, true, || Some(lease(3))).action,
            Some(GestureAction::Start(lease(3)))
        );
    }
}
