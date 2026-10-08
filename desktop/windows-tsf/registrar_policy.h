#pragma once

// Pure cleanup gate shared by the real registrar and a failure-injection probe.
// -1 means the public TSF state query failed; it is never treated as absent.
struct ProbeOwnedState {
  int profile = -1;
  int category = -1;
  int active = -1;
  bool com_owned = false;
  bool com_conflict = false;
};

constexpr bool probe_state_observed(const ProbeOwnedState &state) {
  return state.profile >= 0 && state.category >= 0 && state.active >= 0 &&
         !state.com_conflict;
}

constexpr bool probe_can_remove_com(const ProbeOwnedState &state,
                                    bool operations_ok) {
  return operations_ok && state.com_owned && probe_state_observed(state) &&
         state.profile == 0 && state.category == 0 && state.active == 0;
}
