#ifndef VOICETYPE_CAPSLOCK_H
#define VOICETYPE_CAPSLOCK_H
#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <fcitx/event.h>
#include <fcitx/instance.h>

namespace voicetype {
struct CapsRestore {
    std::string display;
    uint32_t keycode = 0;
    bool locked = false;
};
// One bounded physical gesture. Never retains text or an InputContext pointer.
class CapsLockGesture {
public:
    static constexpr uint64_t LeaseUsec = 2000000;
    enum class Begin { Started, Repeat, Rejected };
    Begin begin(const std::string &display, uint32_t keycode, bool locked,
                uint32_t timestamp, uint64_t now);
    std::optional<CapsRestore> release(const std::string &display, uint32_t keycode,
                uint32_t timestamp, uint64_t now, bool physical = true);
    bool isLaterPress(const std::string &display, uint32_t keycode,
                      uint32_t timestamp) const;
    void expire(uint64_t now);
    void clear() { active_ = false; }
    bool active() const { return active_; }
    uint64_t deadline() const { return started_ + LeaseUsec; }
    const CapsRestore &saved() const { return saved_; }
private:
    CapsRestore saved_;
    uint32_t timestamp_ = 0;
    uint64_t started_ = 0;
    bool active_ = false;
};

class CapsLockGuard {
public:
    explicit CapsLockGuard(fcitx::Instance *instance);
    ~CapsLockGuard();
    // Called for every key before normal processing. True consumes a matching
    // release or a repeat of the already-armed shortcut (no second learning).
    bool filter(fcitx::KeyEvent &event, bool shortcutPress);
    bool enabled() const;
private:
    class Impl;
    std::unique_ptr<Impl> impl_;
};
}
#endif
