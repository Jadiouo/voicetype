#include "registrar_policy.h"
#include <cstdio>

int main() {
  const ProbeOwnedState clean{0, 0, 0, true, false};
  if (!probe_can_remove_com(clean, true)) return 1;
  // A failed public API operation, residual profile/category/activation,
  // failed observation, or external modification must retain the COM owner.
  if (probe_can_remove_com(clean, false) ||
      probe_can_remove_com({1, 0, 0, true, false}, true) ||
      probe_can_remove_com({0, 1, 0, true, false}, true) ||
      probe_can_remove_com({0, 0, 1, true, false}, true) ||
      probe_can_remove_com({-1, 0, 0, true, false}, true) ||
      probe_can_remove_com({0, -1, 0, true, false}, true) ||
      probe_can_remove_com({0, 0, -1, true, false}, true) ||
      probe_can_remove_com({0, 0, 0, false, false}, true) ||
      probe_can_remove_com({0, 0, 0, true, true}, true)) return 1;
  std::puts("PASS: injected rollback/query failures retain COM ownership");
  return 0;
}
