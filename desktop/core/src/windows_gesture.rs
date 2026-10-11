//! Finite toggle gesture state, independent of Win32 message delivery.
use crate::{ShortcutBinding, ShortcutChoice, TargetLease};

pub(crate) fn start_binding<T>(
    binding: ShortcutBinding,
    mut start: impl FnMut(ShortcutChoice) -> std::io::Result<T>,
) -> std::io::Result<(T, ShortcutChoice)> {
    match start(binding.requested) {
        Ok(owner) => Ok((owner, binding.requested)),
        Err(error) if binding.requested != binding.effective => start(binding.effective)
            .map(|owner| (owner, binding.effective))
            .map_err(|_| error),
        Err(error) => Err(error),
    }
}

pub(crate) fn shortcut_matches(
    choice: ShortcutChoice,
    ctrl: bool,
    shift: bool,
    alt: bool,
    windows: bool,
) -> bool {
    match choice {
        ShortcutChoice::Default => ctrl,
        ShortcutChoice::CtrlShiftF9 => ctrl && shift && !alt && !windows,
    }
}

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
    shortcut_latched: bool,
    escape_latched: bool,
    escape_consumed: bool,
    owner_active: bool,
}

impl WindowsGesture {
    #[cfg(windows)]
    pub fn needs_target(&self) -> bool {
        matches!(self.phase, Phase::Idle) && !self.shortcut_latched
    }

    pub fn caps(
        &mut self,
        down: bool,
        ctrl: bool,
        target: impl FnOnce() -> Option<TargetLease>,
    ) -> KeyOutcome {
        self.trigger(down, ctrl, target)
    }

    pub fn trigger(
        &mut self,
        down: bool,
        matched: bool,
        target: impl FnOnce() -> Option<TargetLease>,
    ) -> KeyOutcome {
        let suppress = self.shortcut_latched || matched;
        if !down {
            self.shortcut_latched = false;
            return KeyOutcome {
                action: None,
                suppress,
            };
        }
        if !matched || self.shortcut_latched {
            return KeyOutcome {
                action: None,
                suppress,
            };
        }
        self.shortcut_latched = true;
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
    fn alternate_chord_toggles_once_per_press_and_leaves_other_f9_keys_alone() {
        let mut gesture = WindowsGesture::default();
        assert!(!gesture.trigger(true, false, || Some(lease(99))).suppress);
        assert!(gesture.trigger(false, false, || None).action.is_none());
        assert_eq!(
            gesture.trigger(true, true, || Some(lease(1))).action,
            Some(GestureAction::Start(lease(1)))
        );
        assert!(gesture
            .trigger(true, true, || Some(lease(2)))
            .action
            .is_none());
        assert!(gesture.trigger(false, false, || None).suppress);
        gesture.observe_owner(false); // queued Start must survive old idle poll
        gesture.observe_owner(true);
        assert_eq!(
            gesture.trigger(true, true, || None).action,
            Some(GestureAction::Stop)
        );
        gesture.trigger(false, true, || None);
        assert!(gesture
            .trigger(true, true, || Some(lease(2)))
            .action
            .is_none());
        gesture.trigger(false, true, || None);
        gesture.observe_owner(false);
        assert_eq!(
            gesture.trigger(true, true, || Some(lease(2))).action,
            Some(GestureAction::Start(lease(2)))
        );
    }

    #[test]
    fn alternate_requires_exact_modifiers_and_default_keeps_ctrl_caps_behavior() {
        assert!(shortcut_matches(
            ShortcutChoice::Default,
            true,
            false,
            false,
            false
        ));
        assert!(!shortcut_matches(
            ShortcutChoice::CtrlShiftF9,
            true,
            false,
            false,
            false
        ));
        assert!(!shortcut_matches(
            ShortcutChoice::CtrlShiftF9,
            false,
            true,
            false,
            false
        ));
        assert!(shortcut_matches(
            ShortcutChoice::CtrlShiftF9,
            true,
            true,
            false,
            false
        ));
        assert!(!shortcut_matches(
            ShortcutChoice::CtrlShiftF9,
            true,
            true,
            true,
            false
        ));
        assert!(!shortcut_matches(
            ShortcutChoice::CtrlShiftF9,
            true,
            true,
            false,
            true
        ));
    }

    #[test]
    fn failed_new_hook_retries_the_last_confirmed_key_without_marking_new_effective() {
        let binding = crate::ShortcutBinding {
            requested: ShortcutChoice::CtrlShiftF9,
            effective: ShortcutChoice::Default,
        };
        let mut attempts = Vec::new();
        let (owner, actual) = start_binding(binding, |choice| {
            attempts.push(choice);
            if choice == ShortcutChoice::CtrlShiftF9 {
                Err(std::io::ErrorKind::PermissionDenied.into())
            } else {
                Ok("old-hook")
            }
        })
        .unwrap();
        assert_eq!(owner, "old-hook");
        assert_eq!(actual, ShortcutChoice::Default);
        assert_eq!(
            attempts,
            vec![ShortcutChoice::CtrlShiftF9, ShortcutChoice::Default]
        );
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
