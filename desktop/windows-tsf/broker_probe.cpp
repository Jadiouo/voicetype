// Deterministic in-process protocol probe. Does not claim pipe ACL or OS CAS.
#include "broker_protocol.h"
#include <cstdio>
using namespace voicetype_tsf;
int main() {
  Header offer{sizeof(Header), version, static_cast<uint32_t>(Kind::final),
               0, 11, 22, 33, 44, 55};
  if (!valid(offer)) return 1;
  DeliveryCell first{11, 22, 44, 55};
  if (!first.same_offer(offer) || !first.same_offer(offer)) return 2;
  offer.text_digest = 56;
  if (first.same_offer(offer)) return 3; // same ID, different text is invalid
  offer.text_digest = 55;
  if (!first.cancel() || first.begin_commit()) return 4;
  DeliveryCell second{11, 22, 44, 55};
  if (!second.begin_commit() || second.cancel() || second.begin_commit()) return 5;
  offer.broker_nonce = 12;
  if (second.same_offer(offer)) return 6; // new broker cannot reuse old token
  offer.broker_nonce = 11;
  offer.frame_bytes = sizeof(Header) + maximum_payload_bytes + 1;
  if (valid(offer)) return 7;
  std::puts("PASS: bounded versioned frame, duplicate digest and cancel/commit state");
  return 0;
}
