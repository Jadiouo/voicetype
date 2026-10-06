// Dispatch real Fcitx events against an owned text client and a fake Unix IPC
// daemon. No desktop frontend, microphone, live service or user config is used.
#include "voicetype.h"

#include <fcitx-utils/utf8.h>
#include <fcitx/surroundingtext.h>
#include <fcitx/inputpanel.h>
#include <fcitx/focusgroup.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <functional>
#include <vector>

using namespace voicetype;
static int failures = 0;
#define CHECK(condition) do { if (!(condition)) { \
    std::fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #condition); \
    ++failures; } } while (0)

struct TextState {
    std::string content;
    unsigned commits = 0;
};

class TextClient : public fcitx::InputContext {
public:
    TextClient(fcitx::Instance &instance, TextState &state)
        : fcitx::InputContext(instance.inputContextManager(), "delivery-test"), state_(state) {
        setCapabilityFlags(fcitx::CapabilityFlag::SurroundingText);
        created();
    }
    ~TextClient() override { destroy(); }
    const char *frontend() const override { return "voicetype-delivery-test"; }
    void setText(const std::string &text, size_t anchor = std::string::npos) {
        state_.content = text;
        const auto cursor = fcitx::utf8::length(text);
        surroundingText().setText(text, cursor, anchor == std::string::npos ? cursor : anchor);
        updateSurroundingText();
    }
    std::function<void()> onPreedit;
protected:
    void commitStringImpl(const std::string &text) override {
        ++state_.commits;
        setText(state_.content + text);
    }
    void deleteSurroundingTextImpl(int, unsigned int) override {}
    void forwardKeyImpl(const fcitx::ForwardKeyEvent &) override {}
    void updatePreeditImpl() override {
        if (onPreedit) {
            auto callback = std::move(onPreedit);
            callback();
        }
    }
private:
    TextState &state_;
};

static void key(fcitx::Instance &instance, TextClient &client,
                const fcitx::Key &value, bool release = false) {
    fcitx::KeyEvent event(&client, value, release);
    instance.postEvent(event);
}

int main(int argc, char **argv) {
    const std::string requested = argc > 1 ? argv[1] : "focus_return";
    const bool appDelivery = requested.rfind("app_", 0) == 0;
    const std::string scenario = requested == "app_ack" ? "same_context" :
        (appDelivery ? requested.substr(4) : requested);
    char temporary[] = "/tmp/voicetype-delivery-XXXXXX";
    const char *dir = mkdtemp(temporary);
    if (!dir) { return EXIT_FAILURE; }
    const std::string path = std::string(dir) + "/ipc.sock";
    setenv("VOICETYPE_SOCKET", path.c_str(), 1);
    setenv("FCITX_CONFIG_HOME", dir, 1);
    setenv("FCITX_DATA_HOME", dir, 1);
    unsetenv("DISPLAY");
    unsetenv("WAYLAND_DISPLAY");
    unsetenv("DBUS_SESSION_BUS_ADDRESS");
    const int listener = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
    sockaddr_un address{};
    address.sun_family = AF_UNIX;
    std::memcpy(address.sun_path, path.c_str(), path.size() + 1);
    CHECK(bind(listener, reinterpret_cast<sockaddr *>(&address), sizeof(address)) == 0);
    CHECK(listen(listener, 1) == 0);
    {
        char name[] = "voicetype-delivery-test";
        char disable[] = "--disable=all";
        char *args[] = {name, disable, nullptr};
        fcitx::Instance instance(2, args);
        instance.initialize();
        VoiceType addon(&instance, [](const std::string &, const std::string &) {}, {},
                        VoiceTypeConfig{});
        fcitx::FocusGroup group("voicetype-test:main", instance.inputContextManager());
        TextState state;
        auto client = std::make_unique<TextClient>(instance, state);
        const bool groupless = scenario.rfind("groupless_", 0) == 0;
        if (!groupless) { client->setFocusGroup(&group); }
        TextState otherState;
        TextClient other(instance, otherState);
        other.focusIn();
        client->focusIn();
        client->setText("existing ");
        int peer = accept4(listener, nullptr, nullptr, SOCK_NONBLOCK | SOCK_CLOEXEC);
        CHECK(peer >= 0);
        std::vector<IpcMessage> received;
        unsigned preeditCallbacks = 0;
        const std::string transcript = "dictated gthub";
        const bool shouldDeliver = scenario == "same_context" || scenario == "duplicate" ||
            scenario == "old_session" || scenario == "unrelated_reset" || scenario == "unrelated_focus_out";
        const bool preeditCase = scenario.rfind("preedit_", 0) == 0;
        const bool disconnectCase = scenario.rfind("disconnect_", 0) == 0;
        const bool recordingCase = scenario.find("_recording") != std::string::npos;
        const bool destroyCase = scenario.find("destroy") != std::string::npos;
        const bool shouldFallback = scenario == "groupless_destroy_result" && !appDelivery;
        const bool expectsCancel = !shouldDeliver && !preeditCase && !disconnectCase &&
            scenario != "error_result" && scenario != "error_destroy_result" && scenario != "empty_result" &&
            scenario != "unfocused_start" && scenario != "wrong_context" &&
            (!groupless || scenario == "groupless_cancel_destroy");
        std::vector<std::string> boundaries;
        const auto *originalClient = client.get();
        auto focusObserver = instance.watchEvent(fcitx::EventType::InputContextFocusOut,
            fcitx::EventWatcherPhase::PreInputMethod, [&](fcitx::Event &event) {
                if (static_cast<fcitx::InputContextEvent &>(event).inputContext() == originalClient) {
                    boundaries.push_back("focus_out");
                }
            });
        auto destroyObserver = instance.watchEvent(fcitx::EventType::InputContextDestroyed,
            fcitx::EventWatcherPhase::PreInputMethod, [&](fcitx::Event &event) {
                if (static_cast<fcitx::InputContextEvent &>(event).inputContext() == originalClient) {
                    boundaries.push_back("destroy");
                }
            });
        std::string incoming;
        const auto count = [&](const std::string &type, uint64_t session = 0) {
            return std::count_if(received.begin(), received.end(), [&](const IpcMessage &message) {
                return message.type == type && (!session || message.session == session);
            });
        };
        const auto reply = [&](const std::string &type, uint64_t session, const std::string &text) {
            IpcMessage message;
            message.type = type;
            message.setSession(session);
            message.text = text;
            if (type == "deliver") {
                const auto start = std::find_if(received.begin(), received.end(), [&](const IpcMessage &m) {
                    return m.type == "start" && m.session == session;
                });
                CHECK(start != received.end());
                if (start != received.end()) {
                    message.contextId = scenario == "wrong_context" ? "other-field" : start->contextId;
                }
            }
            const auto wire = serialize(message) + "\n";
            CHECK(send(peer, wire.data(), wire.size(), MSG_NOSIGNAL) == static_cast<ssize_t>(wire.size()));
        };
        const auto record = [&]() {
            key(instance, *client, fcitx::Key(FcitxKey_Alt_L, fcitx::KeyState::Ctrl));
            key(instance, *client, fcitx::Key(FcitxKey_Alt_L), true);
        };
        std::vector<std::function<bool()>> steps;
        const auto action = [&](std::function<void()> fn) {
            steps.push_back([fn = std::move(fn)]() { fn(); return true; });
        };
        const auto settle = [&]() {
            steps.push_back([until = uint64_t{0}]() mutable {
                const auto now = fcitx::now(CLOCK_MONOTONIC);
                if (!until) { until = now + 100000; }
                return now >= until;
            });
        };
        if (appDelivery) {
            action([&]() { reply("desktop_hello", 991, ""); });
            steps.push_back([&]() { return count("desktop_hello", 991) == 1; });
        }
        action([&]() {
            if (scenario == "unfocused_start") {
                client->focusOut();
                record();
                client->focusIn();
            } else if (recordingCase) {
                key(instance, *client, fcitx::Key(FcitxKey_Alt_L, fcitx::KeyState::Ctrl));
            } else { record(); }
        });
        if (scenario == "unfocused_start") { settle(); }
        else { steps.push_back([&]() { return count(recordingCase ? "start" : "stop", 1) == 1; }); }
        action([&]() {
            CHECK(count("start", 1) == (scenario == "unfocused_start" ? 0 : 1));
            if (scenario == "reset" || scenario == "reset_recording") {
                client->reset();
            } else if (scenario == "sensitive_return" || scenario == "sensitive_recording" ||
                       scenario == "password_return") {
                client->setCapabilityFlags(fcitx::CapabilityFlags(fcitx::CapabilityFlag::SurroundingText) |
                    (scenario == "password_return" ? fcitx::CapabilityFlag::Password : fcitx::CapabilityFlag::Sensitive));
                client->setCapabilityFlags(fcitx::CapabilityFlag::SurroundingText);
            } else if (scenario == "cancel_processing" || scenario == "cancel_recording" ||
                       scenario == "cancel_destroy" || scenario == "groupless_cancel_destroy") {
                key(instance, *client, fcitx::Key(FcitxKey_Escape));
                if (destroyCase) { client.reset(); }
            } else if (disconnectCase) {
                close(peer);
                peer = -1;
            } else if (scenario == "error_result" || scenario == "error_destroy_result") {
                reply("error", 1, "ASR failed");
            } else if (scenario == "empty_result") {
                reply("result", 1, "");
            } else if (preeditCase) {
                client->setCapabilityFlags(fcitx::CapabilityFlags(fcitx::CapabilityFlag::SurroundingText) |
                                           fcitx::CapabilityFlag::Preedit);
                client->inputPanel().setPreedit(fcitx::Text("composing"));
                client->inputPanel().setClientPreedit(fcitx::Text("composing"));
                client->updatePreedit();
                client->onPreedit = [&]() {
                    ++preeditCallbacks;
                    if (scenario == "preedit_reset") { client->reset(); }
                    else if (scenario == "preedit_focus_return") { client->focusOut(); client->focusIn(); }
                    else if (scenario == "preedit_sensitive_return") {
                        client->setCapabilityFlags(fcitx::CapabilityFlag::Sensitive);
                        client->setCapabilityFlags(fcitx::CapabilityFlag::SurroundingText);
                    } else if (scenario == "preedit_new_session") { record(); }
                };
            } else if (scenario == "unrelated_reset") {
                other.reset();
                CHECK(client->hasFocus());
            } else if (scenario == "unrelated_focus_out") {
                other.focusOut();
                CHECK(client->hasFocus());
            } else if (scenario == "old_session") {
                record();
            } else if (scenario == "destroy_result" || scenario == "destroy_error_result" ||
                       scenario == "groupless_destroy_result" || scenario == "groupless_destroy_error_result") {
                client.reset();
                if (scenario.find("destroy_error_result") != std::string::npos) { reply("error", 1, "ASR failed"); }
            } else if (scenario == "focus_out" || scenario == "focus_return" || scenario == "focus_recording") {
                client->focusOut();
                if (scenario != "focus_out") { client->focusIn(); }
            } else {
                CHECK(appDelivery || scenario == "same_context" || scenario == "duplicate" || scenario == "unfocused_start");
            }
        });
        if (disconnectCase) {
            steps.push_back([&]() {
                peer = accept4(listener, nullptr, nullptr, SOCK_NONBLOCK | SOCK_CLOEXEC);
                return peer >= 0;
            });
        }
        if (scenario == "error_destroy_result") { settle(); }
        action([&]() {
            if (scenario == "disconnect_destroy" || scenario == "error_destroy_result") { client.reset(); }
            if (scenario == "old_session") {
                reply("error", 1, "stale error");
                reply("result", 1, "stale result");
                reply("result", 2, transcript);
            } else if (appDelivery) {
                reply("deliver", 1, transcript);
                reply("deliver", 1, transcript);
            } else {
                reply("result", 1, transcript);
                if (scenario == "duplicate" || shouldFallback) { reply("result", 1, transcript); }
            }
        });
        settle();
        action([&]() {
            CHECK(state.commits == (shouldDeliver ? 1u : 0u));
            CHECK(state.content == (shouldDeliver ? "existing " + transcript : "existing "));
            CHECK(count("fallback_clipboard") == (shouldFallback ? 1 : 0));
            CHECK(otherState.commits == 0);
            if (appDelivery) {
                std::vector<std::string> outcomes;
                for (const auto &message : received) {
                    if (message.type == "delivered" && message.session == 1) { outcomes.push_back(message.code); }
                    if (message.type == "desktop_hello") { CHECK(message.value == "voicetype.fcitx.v1"); }
                }
                const std::string first = shouldDeliver ? "committed" :
                    ((preeditCase || scenario == "wrong_context" || scenario == "groupless_destroy_result") ?
                        "focus_changed" : "stale");
                CHECK((outcomes == std::vector<std::string>{first, "stale"}));
            }
            if (preeditCase) { CHECK(preeditCallbacks == 1); }
            CHECK(count("cancel", 1) == (expectsCancel ? 1 : 0));
            if (destroyCase) {
                // A focused IC in a FocusGroup emits focus-out before destroy.
                // Without a group, Fcitx emits only destroy: the uncanceled
                // weak-target loss is the supported one-shot fallback path.
                CHECK(boundaries == (groupless ? std::vector<std::string>{"destroy"} :
                                     std::vector<std::string>{"focus_out", "destroy"}));
                if (shouldFallback) {
                    const auto fallback = std::find_if(received.begin(), received.end(), [](const IpcMessage &m) {
                        return m.type == "fallback_clipboard";
                    });
                    CHECK(fallback != received.end() && fallback->text == transcript);
                }
            }
        });
        if (scenario == "preedit_new_session") {
            steps.push_back([&]() { return count("stop", 2) == 1; });
            action([&]() { reply("result", 2, transcript); });
            steps.push_back([&]() { return state.commits == 1; });
        }
        if (shouldDeliver || scenario == "preedit_new_session") {
            action([&]() {
                CHECK(state.content == "existing " + transcript);
                client->setText("existing dictated GitHub", fcitx::utf8::length("existing "));
                key(instance, *client, fcitx::Key("Control+Caps_Lock"));
            });
            steps.push_back([&]() { return count("correction") == 1; });
            action([&]() {
                const uint64_t session = scenario == "old_session" || scenario == "preedit_new_session" ? 2 : 1;
                const auto correction = std::find_if(received.begin(), received.end(), [](const IpcMessage &m) {
                    return m.type == "correction";
                });
                const auto start = std::find_if(received.begin(), received.end(), [&](const IpcMessage &m) {
                    return m.type == "start" && m.session == session;
                });
                CHECK(correction != received.end() && start != received.end());
                if (correction != received.end() && start != received.end()) {
                    CHECK(correction->session == session);
                    CHECK(correction->contextId == start->contextId);
                    CHECK(correction->before == transcript && correction->after == "dictated GitHub");
                    CHECK(correction->hasConfirmed && correction->confirmed);
                }
                CHECK(count("fallback_clipboard") == 0);
                CHECK(state.commits == 1);
            });
        }

        auto &loop = instance.eventLoop();
        size_t stage = 0;
        auto timer = loop.addTimeEvent(CLOCK_MONOTONIC, fcitx::now(CLOCK_MONOTONIC) + 1000, 0,
            [&](fcitx::EventSourceTime *event, uint64_t) {
                char buffer[4096];
                ssize_t n;
                while ((n = read(peer, buffer, sizeof(buffer))) > 0) {
                    incoming.append(buffer, static_cast<size_t>(n));
                }
                size_t end;
                while ((end = incoming.find('\n')) != std::string::npos) {
                    IpcMessage message;
                    CHECK(parse(incoming.substr(0, end), message));
                    incoming.erase(0, end + 1);
                    received.push_back(std::move(message));
                }
                if (stage < steps.size() && steps[stage]()) { ++stage; }
                if (stage == steps.size()) { loop.exit(); return false; }
                event->setTime(fcitx::now(CLOCK_MONOTONIC) + 5000);
                event->setOneShot();
                return true;
            });
        auto timeout = loop.addTimeEvent(CLOCK_MONOTONIC, fcitx::now(CLOCK_MONOTONIC) + 5000000, 0,
            [&](fcitx::EventSourceTime *, uint64_t) {
                std::fprintf(stderr, "delivery %s timeout: stage=%zu commits=%u\n",
                             scenario.c_str(), stage, state.commits);
                ++failures;
                loop.exit();
                return false;
            });
        loop.exec();
        close(peer);
    }
    close(listener);
    std::filesystem::remove_all(dir);
    return failures ? EXIT_FAILURE : EXIT_SUCCESS;
}
