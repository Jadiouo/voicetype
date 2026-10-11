#pragma once
#include <atomic>
#include <cstdint>

// Fixed-width prototype for the future private pipe. No pointers, handles or
// transcript bytes are interpreted here. This is not a pipe/security test.
namespace voicetype_tsf {
constexpr uint32_t version = 1;
constexpr uint32_t maximum_payload_bytes = 64 * 1024;
enum class Kind : uint32_t { hello = 1, start = 2, final = 3,
                              cancel = 4, ack = 5 };
struct Header {
  uint32_t frame_bytes;
  uint32_t protocol_version;
  uint32_t kind;
  uint32_t reserved;
  uint64_t broker_nonce;
  uint64_t service_nonce;
  uint64_t session;
  uint64_t delivery;
  uint64_t text_digest;
};
static_assert(sizeof(Header) == 56);

inline bool valid(const Header &h) {
  return h.frame_bytes >= sizeof(Header) &&
         h.frame_bytes <= sizeof(Header) + maximum_payload_bytes &&
         h.protocol_version == version && h.reserved == 0 &&
         h.broker_nonce && h.service_nonce && h.session &&
         h.kind >= static_cast<uint32_t>(Kind::hello) &&
         h.kind <= static_cast<uint32_t>(Kind::ack);
}

enum class Phase : uint32_t { pending, cancelled, committing,
                              committed, unconfirmed };
struct DeliveryCell {
  const uint64_t broker_nonce;
  const uint64_t service_nonce;
  const uint64_t delivery;
  const uint64_t digest;
  std::atomic<Phase> phase{Phase::pending};
  bool same_offer(const Header &h) const {
    return valid(h) && h.kind == static_cast<uint32_t>(Kind::final) &&
           h.broker_nonce == broker_nonce && h.service_nonce == service_nonce &&
           h.delivery == delivery && h.text_digest == digest;
  }
  bool cancel() {
    auto expected = Phase::pending;
    return phase.compare_exchange_strong(expected, Phase::cancelled);
  }
  bool begin_commit() {
    auto expected = Phase::pending;
    return phase.compare_exchange_strong(expected, Phase::committing);
  }
};
}  // namespace voicetype_tsf
