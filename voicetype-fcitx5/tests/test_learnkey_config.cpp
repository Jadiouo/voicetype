// Isolated config and key routing test. No desktop frontend or live daemon.
#include "voicetype.h"

#include <fcitx-utils/utf8.h>

#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <string>
#include <utility>
#include <vector>

using namespace voicetype;
static int failures = 0;
#define CHECK(condition) do { if (!(condition)) { \
    std::fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #condition); \
    ++failures; } } while (0)

class TextClient : public fcitx::InputContext {
public:
    explicit TextClient(fcitx::Instance &instance)
        : fcitx::InputContext(instance.inputContextManager(), "learnkey-test") {
        setCapabilityFlags(fcitx::CapabilityFlag::SurroundingText);
    }
    ~TextClient() override { destroy(); }
    const char *frontend() const override { return "learnkey-test"; }
protected:
    void commitStringImpl(const std::string &) override {}
    void deleteSurroundingTextImpl(int, unsigned int) override {}
    void forwardKeyImpl(const fcitx::ForwardKeyEvent &) override {}
    void updatePreeditImpl() override {}
};

static bool post(fcitx::Instance &instance, TextClient &client,
                 const fcitx::Key &key, bool release = false) {
    fcitx::KeyEvent event(&client, key, release);
    instance.postEvent(event);
    return event.accepted();
}

int main() {
    char temporary[] = "/tmp/voicetype-learnkey-XXXXXX";
    const char *dir = mkdtemp(temporary);
    if (!dir) { return EXIT_FAILURE; }
    const std::string socketPath = std::string(dir) + "/ipc.sock";
    setenv("VOICETYPE_SOCKET", socketPath.c_str(), 1);
    setenv("XDG_RUNTIME_DIR", dir, 1);
    setenv("FCITX_CONFIG_HOME", dir, 1);
    setenv("FCITX_DATA_HOME", dir, 1);
    unsetenv("DISPLAY");
    unsetenv("WAYLAND_DISPLAY");

    const int listener = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    sockaddr_un address{};
    address.sun_family = AF_UNIX;
    std::memcpy(address.sun_path, socketPath.c_str(), socketPath.size() + 1);
    CHECK(bind(listener, reinterpret_cast<sockaddr *>(&address), sizeof(address)) == 0);
    CHECK(listen(listener, 1) == 0);
    {
        char name[] = "voicetype-learnkey-test";
        char disable[] = "--disable=all";
        char *argv[] = {name, disable, nullptr};
        fcitx::Instance instance(2, argv);
        instance.initialize();

        VoiceTypeConfig defaults;
        CHECK(defaults.learnKey->toString() == "Control+Caps_Lock");
        VoiceTypeConfig remapped;
        CHECK(remapped.learnKey.setValue(fcitx::Key("Control+Shift+Caps_Lock")));
        std::vector<std::pair<std::string, std::string>> notices;
        VoiceType addon(&instance,
            [&notices](const std::string &summary, const std::string &body) {
                notices.emplace_back(summary, body);
            }, {}, remapped);
        auto *reportedConfig = dynamic_cast<const VoiceTypeConfig *>(addon.getConfig());
        CHECK(reportedConfig && reportedConfig->learnKey->toString() == "Control+Shift+Caps_Lock");

        TextClient client(instance);
        client.focusIn();
        const int peer = accept4(listener, nullptr, nullptr, SOCK_NONBLOCK | SOCK_CLOEXEC);
        CHECK(peer >= 0);

        // With the Shift chord configured, the Google-owned Ctrl+Caps chord
        // must pass through without invoking Nano Learn.
        CHECK(!post(instance, client, fcitx::Key("Control+Caps_Lock")));
        CHECK(notices.empty());

        // Enter recording so the configured Learn chord takes the existing
        // recording guard and reports the active, configured binding.
        CHECK(post(instance, client, fcitx::Key(FcitxKey_Alt_L, fcitx::KeyState::Ctrl)));
        CHECK(!post(instance, client, fcitx::Key("Control+Caps_Lock")));
        CHECK(notices.empty());
        CHECK(post(instance, client, fcitx::Key("Control+Shift+Caps_Lock")));
        CHECK(notices.size() == 1);
        CHECK(notices.back().first == "正在錄音，尚未學習");
        const auto firstLabel = remapped.learnKey->toString(fcitx::KeyStringFormat::Localized);
        CHECK(notices.back().second.find(firstLabel) != std::string::npos);

        // The standard config API persists in this test's private XDG root;
        // reloadConfig must restore it and the new binding must take effect.
        fcitx::RawConfig raw;
        raw["LearnKey"] = "Control+Shift+F11";
        addon.setConfig(raw);
        addon.reloadConfig();
        reportedConfig = dynamic_cast<const VoiceTypeConfig *>(addon.getConfig());
        CHECK(reportedConfig && reportedConfig->learnKey->toString() == "Control+Shift+F11");
        CHECK(!post(instance, client, fcitx::Key("Control+Shift+Caps_Lock")));
        CHECK(notices.size() == 1);
        CHECK(post(instance, client, fcitx::Key("Control+Shift+F11")));
        CHECK(notices.size() == 2);
        const auto reloadedLabel = reportedConfig->learnKey->toString(fcitx::KeyStringFormat::Localized);
        CHECK(notices.back().second.find(reloadedLabel) != std::string::npos);
        CHECK(post(instance, client, fcitx::Key(FcitxKey_Alt_L), true));

        close(peer);
    }
    close(listener);
    std::filesystem::remove_all(dir);
    return failures ? EXIT_FAILURE : EXIT_SUCCESS;
}
