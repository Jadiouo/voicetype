#include "capslock.h"
#include <fcitx-utils/log.h>
#include <fcitx-utils/event.h>
#include <fcitx/addonmanager.h>
#include <unordered_map>
#include <cstdlib>
#ifdef VOICETYPE_HAVE_X11_CAPS_RESTORE
#include <xcb_public.h>
// xcb-xkb 1.15 uses the C field name "explicit"; contain its C++ workaround.
#define explicit explicit_
#include <xcb/xkb.h>
#undef explicit
#endif

namespace voicetype {
CapsLockGesture::Begin CapsLockGesture::begin(const std::string &display,
        uint32_t code, bool locked, uint32_t timestamp, uint64_t now) {
    expire(now);
    if (display.empty() || code < 8 || code > 255 || timestamp == 0) { return Begin::Rejected; }
    if (active_) {
        return saved_.display == display && saved_.keycode == code ? Begin::Repeat : Begin::Rejected;
    }
    saved_ = {display, code, locked}; timestamp_ = timestamp; started_ = now; active_ = true;
    return Begin::Started;
}
bool CapsLockGesture::isLaterPress(const std::string &display, uint32_t code,
                                   uint32_t timestamp) const {
    const auto elapsed = static_cast<uint32_t>(timestamp - timestamp_);
    return active_ && saved_.display == display && saved_.keycode == code &&
           elapsed > 0 && elapsed < LeaseUsec / 1000;
}
void CapsLockGesture::expire(uint64_t now) {
    if (active_ && (now < started_ || now - started_ >= LeaseUsec)) { clear(); }
}
std::optional<CapsRestore> CapsLockGesture::release(const std::string &display,
        uint32_t code, uint32_t timestamp, uint64_t now, bool physical) {
    expire(now);
    if (!active_ || !physical || saved_.display != display || saved_.keycode != code ||
        static_cast<uint32_t>(timestamp - timestamp_) >= LeaseUsec / 1000) { return std::nullopt; }
    auto result = saved_; clear(); return result;
}

class CapsLockGuard::Impl {
public:
    explicit Impl(fcitx::Instance *instance) : instance_(instance) {
#ifdef VOICETYPE_HAVE_X11_CAPS_RESTORE
        // Never load/open an X connection just to guess from DISPLAY. Optional
        // dependency order makes the existing module available in native X11.
        module_ = instance_->addonManager().addon("xcb", false);
        if (!module_) { return; }
        closed_ = module_->call<fcitx::IXCBModule::addConnectionClosedCallback>(
            [this](const std::string &name, xcb_connection_t *) {
                if (gesture_.active() && gesture_.saved().display == name) { gesture_.clear(); }
                connections_.erase(name);
            });
        created_ = module_->call<fcitx::IXCBModule::addConnectionCreatedCallback>(
            [this](const std::string &name, xcb_connection_t *, int, fcitx::FocusGroup *) {
                if (module_->call<fcitx::IXCBModule::isXWayland>(name) ||
                    !module_->call<fcitx::IXCBModule::xkbState>(name)) { return; }
                // Fcitx's internal filter consumes core XKB StateNotify.
                // Use a separate, already-verified native display connection.
                // Startup handshake only; no synchronous work on the key path.
                auto observer = std::make_unique<Connection>();
                observer->connection = xcb_connect(name.c_str(), nullptr);
                if (xcb_connection_has_error(observer->connection)) { return; }
                const auto *extension = xcb_get_extension_data(observer->connection, &xcb_xkb_id);
                if (!extension || !extension->present) { return; }
                auto *reply = xcb_xkb_use_extension_reply(observer->connection,
                    xcb_xkb_use_extension(observer->connection, 1, 0), nullptr);
                const bool supported = reply && reply->supported;
                std::free(reply);
                if (!supported) { return; }
                observer->firstEvent = extension->first_event;
                auto *ptr = observer.get();
                observer->io = instance_->eventLoop().addIOEvent(
                    xcb_get_file_descriptor(observer->connection), fcitx::IOEventFlag::In,
                    [this, name, ptr](fcitx::EventSourceIO *source, int, fcitx::IOEventFlags) {
                        while (auto *event = xcb_poll_for_event(ptr->connection)) {
                            if ((event->response_type & 0x7f) == ptr->firstEvent &&
                                !(event->response_type & 0x80)) {
                                const auto *state = reinterpret_cast<const xcb_xkb_state_notify_event_t *>(event);
                                expire(fcitx::now(CLOCK_MONOTONIC));
                                if (state->xkbType == XCB_XKB_STATE_NOTIFY &&
                                    state->requestMajor == 0 && state->requestMinor == 0) {
                                    if (state->eventType == XCB_KEY_RELEASE) {
                                        if (auto restore = gesture_.release(name, state->keycode,
                                                state->time, fcitx::now(CLOCK_MONOTONIC))) {
                                            FCITX_DEBUG() << "voicetype caps release: source=xkb keycode=" << static_cast<unsigned>(state->keycode);
                                            request(*restore);
                                            subscribe(name, false);
                                        }
                                    } else if (state->eventType == XCB_KEY_PRESS &&
                                               gesture_.isLaterPress(name, state->keycode, state->time)) {
                                        // An older subscription can leave queued press events.
                                        // Only a strictly later physical press supersedes this
                                        // gesture; its own press and stale events never do.
                                        clearGesture();
                                    }
                                }
                            }
                            std::free(event);
                        }
                        if (xcb_connection_has_error(ptr->connection)) {
                            if (gesture_.active() && gesture_.saved().display == name) { clearGesture(); }
                            ptr->ready = false;
                            source->setEnabled(false);
                            return true;
                        }
                        return true;
                    });
                connections_.insert_or_assign(name, std::move(observer));
            });
        if (!connections_.empty()) { FCITX_INFO() << "voicetype: native X11 Caps Lock preservation enabled"; }
#endif
    }
    ~Impl() {
        // Never write a saved state at destruction or after lease expiry.
        gesture_.clear(); timer_.reset();
#ifdef VOICETYPE_HAVE_X11_CAPS_RESTORE
        created_.reset(); closed_.reset(); connections_.clear();
#endif
    }
    bool enabled() const {
#ifdef VOICETYPE_HAVE_X11_CAPS_RESTORE
        return !connections_.empty();
#else
        return false;
#endif
    }
    bool filter(fcitx::KeyEvent &event, bool shortcutPress) {
#ifdef VOICETYPE_HAVE_X11_CAPS_RESTORE
        const auto now = fcitx::now(CLOCK_MONOTONIC);
        expire(now);
        const auto display = event.inputContext()->display();
        if (shortcutPress) {
            FCITX_DEBUG() << "voicetype caps shortcut: display=" << display
                         << " keycode=" << event.origKey().code()
                         << " time=" << static_cast<uint32_t>(event.time())
                         << " rawLock=" << event.origKey().states().test(fcitx::KeyState::CapsLock);
        }
        if (display.rfind("x11:", 0) != 0 || event.isVirtual()) { return false; }
        const auto name = display.substr(4);
        const auto conn = connections_.find(name);
        if (conn == connections_.end() || !conn->second->ready || !module_->call<fcitx::IXCBModule::exists>(name) ||
            module_->call<fcitx::IXCBModule::isXWayland>(name)) { return false; }
        const auto raw = event.origKey();
        if (raw.sym() != FcitxKey_Caps_Lock || raw.code() < 8 || raw.code() > 255) { return false; }
        if (event.isRelease()) {
            if (auto restore = gesture_.release(name, raw.code(), static_cast<uint32_t>(event.time()), now)) {
                FCITX_DEBUG() << "voicetype caps release: source=frontend keycode=" << raw.code();
                request(*restore); subscribe(name, false); return true;
            }
            return false;
        }
        if (!shortcutPress) {
            // An ordinary later Caps press belongs to the user, not an old
            // shortcut whose release went missing. Do not restore over it.
            if (gesture_.active() && gesture_.saved().display == name) { clearGesture(); }
            return false;
        }
        // Repeat events never re-arm an expired held gesture or learn twice.
        if (raw.states().test(fcitx::KeyState::Repeat)) { return true; }
        const auto result = gesture_.begin(name, raw.code(), raw.states().test(fcitx::KeyState::CapsLock),
                                           static_cast<uint32_t>(event.time()), now);
        if (result == CapsLockGesture::Begin::Repeat) { return true; }
        if (result != CapsLockGesture::Begin::Started) { return false; }
        // Selection and restoration share one client FIFO: no cross-connection
        // race in which a release is processed before subscription takes effect.
        subscribe(name, true);
        request(gesture_.saved());
        const auto deadline = gesture_.deadline();
        if (!timer_) {
            timer_ = instance_->eventLoop().addTimeEvent(CLOCK_MONOTONIC, deadline, 1,
                [this](fcitx::EventSourceTime *event, uint64_t) {
                    // Always inspect the current gesture. An old timer can
                    // never clear a newer gesture or send a delayed restore.
                    expire(fcitx::now(CLOCK_MONOTONIC));
                    if (gesture_.active()) {
                        event->setTime(gesture_.deadline()); event->setOneShot(); return true;
                    }
                    event->setEnabled(false);
                    return true;
                });
        } else { timer_->setTime(deadline); timer_->setOneShot(); }
#else
        (void)event; (void)shortcutPress;
#endif
        return false;
    }
private:
    void expire(uint64_t now) {
#ifdef VOICETYPE_HAVE_X11_CAPS_RESTORE
        const auto name = gesture_.active() ? gesture_.saved().display : std::string();
        gesture_.expire(now);
        if (!name.empty() && !gesture_.active()) { subscribe(name, false); }
#else
        (void)now;
#endif
    }
    void clearGesture() {
#ifdef VOICETYPE_HAVE_X11_CAPS_RESTORE
        if (gesture_.active()) { subscribe(gesture_.saved().display, false); }
#endif
        gesture_.clear();
    }
    void subscribe(const std::string &name, bool enabled) {
#ifdef VOICETYPE_HAVE_X11_CAPS_RESTORE
        const auto it = connections_.find(name);
        if (it == connections_.end() || !it->second->ready) { return; }
        auto *connection = it->second->connection;
        if (enabled) {
            xcb_xkb_select_events_details_t details{};
            details.affectState = details.stateDetails = XCB_XKB_STATE_PART_MODIFIER_STATE |
                XCB_XKB_STATE_PART_MODIFIER_BASE | XCB_XKB_STATE_PART_MODIFIER_LOCK;
            xcb_xkb_select_events_aux(connection, XCB_XKB_ID_USE_CORE_KBD,
                XCB_XKB_EVENT_TYPE_STATE_NOTIFY, 0, 0, 0, 0, &details);
        } else {
            xcb_xkb_select_events(connection, XCB_XKB_ID_USE_CORE_KBD,
                XCB_XKB_EVENT_TYPE_STATE_NOTIFY, XCB_XKB_EVENT_TYPE_STATE_NOTIFY, 0, 0, 0, nullptr);
        }
        xcb_flush(connection);
#else
        (void)name; (void)enabled;
#endif
    }
    void request(const CapsRestore &restore) {
#ifdef VOICETYPE_HAVE_X11_CAPS_RESTORE
        const auto it = connections_.find(restore.display);
        if (it == connections_.end() || xcb_connection_has_error(it->second->connection)) { return; }
        // No reply wait and no remapping: change only the real Lock modifier.
        xcb_xkb_latch_lock_state(it->second->connection, XCB_XKB_ID_USE_CORE_KBD,
                                XCB_MOD_MASK_LOCK, restore.locked ? XCB_MOD_MASK_LOCK : 0,
                                0, 0, 0, 0, 0);
        xcb_flush(it->second->connection);
#else
        (void)restore;
#endif
    }
    fcitx::Instance *instance_;
    CapsLockGesture gesture_;
    std::unique_ptr<fcitx::EventSourceTime> timer_;
#ifdef VOICETYPE_HAVE_X11_CAPS_RESTORE
    struct Connection {
        xcb_connection_t *connection = nullptr;
        uint8_t firstEvent = 0;
        bool ready = true;
        std::unique_ptr<fcitx::EventSourceIO> io;
        ~Connection() { io.reset(); if (connection) { xcb_disconnect(connection); } }
    };
    fcitx::AddonInstance *module_ = nullptr;
    std::unordered_map<std::string, std::unique_ptr<Connection>> connections_;
    std::unique_ptr<fcitx::HandlerTableEntry<fcitx::XCBConnectionCreated>> created_;
    std::unique_ptr<fcitx::HandlerTableEntry<fcitx::XCBConnectionClosed>> closed_;
#endif
};
CapsLockGuard::CapsLockGuard(fcitx::Instance *instance) : impl_(std::make_unique<Impl>(instance)) {}
CapsLockGuard::~CapsLockGuard() = default;
bool CapsLockGuard::filter(fcitx::KeyEvent &event, bool shortcutPress) { return impl_->filter(event, shortcutPress); }
bool CapsLockGuard::enabled() const { return impl_->enabled(); }
}
