// Notifications disabled: test real Fcitx auxiliary fallback without a desktop
// popup, changing the user's input-method state, or contacting a daemon.
#include "voicetype.h"
#include <fcitx/inputpanel.h>

#include <cstdio>
#include <cstdlib>
#include <filesystem>

class TextClient : public fcitx::InputContext {
public:
    explicit TextClient(fcitx::Instance &instance)
        : fcitx::InputContext(instance.inputContextManager(), "feedback-test") {}
    ~TextClient() override { destroy(); }
    const char *frontend() const override { return "feedback-test"; }
protected:
    void commitStringImpl(const std::string &) override {}
    void deleteSurroundingTextImpl(int, unsigned int) override {}
    void forwardKeyImpl(const fcitx::ForwardKeyEvent &) override {}
    void updatePreeditImpl() override {}
};

static int failures = 0;
#define CHECK(condition) do { if (!(condition)) { \
    std::fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #condition); \
    ++failures; } } while (0)

static void learn(fcitx::Instance &instance, TextClient &client) {
    fcitx::KeyEvent event(&client, fcitx::Key("Control+Caps_Lock"));
    instance.postEvent(event);
    CHECK(event.accepted());
}

int main() {
    char temporary[] = "/tmp/voicetype-feedback-XXXXXX";
    const char *dir = mkdtemp(temporary);
    if (!dir) { return EXIT_FAILURE; }
    setenv("VOICETYPE_SOCKET", (std::string(dir) + "/missing.sock").c_str(), 1);
    setenv("XDG_RUNTIME_DIR", dir, 1);
    setenv("FCITX_CONFIG_HOME", dir, 1);
    setenv("FCITX_DATA_HOME", dir, 1);
    {
        char name[] = "voicetype-feedback-test";
        char disable[] = "--disable=all";
        char *argv[] = {name, disable, nullptr};
        fcitx::Instance instance(2, argv);
        instance.initialize();
        auto client = std::make_unique<TextClient>(instance);
        client->focusIn();
        client->inputPanel().setPreedit(fcitx::Text("仍在組字"));
        client->inputPanel().setAuxDown(fcitx::Text("原本候選提示"));
        {
            voicetype::VoiceType addon(&instance, {}, {}, voicetype::VoiceTypeConfig{});
            learn(instance, *client);
            CHECK(client->inputPanel().preedit().toString() == "仍在組字");
            CHECK(client->inputPanel().auxDown().toString().find("原本候選提示\n語音服務未連線") == 0);
            learn(instance, *client);
            const auto text = client->inputPanel().auxDown().toString();
            CHECK(text.find("語音服務未連線") == text.rfind("語音服務未連線"));
        }
        // Cleanup restores original auxiliary content and leaves composition.
        CHECK(client->inputPanel().auxDown().toString() == "原本候選提示");
        CHECK(client->inputPanel().preedit().toString() == "仍在組字");
        {
            voicetype::VoiceType addon(&instance, {}, {}, voicetype::VoiceTypeConfig{});
            learn(instance, *client);
            // A newer input-method update belongs to that input method, not us.
            client->inputPanel().setAuxDown(fcitx::Text("新的候選提示"));
        }
        CHECK(client->inputPanel().auxDown().toString() == "新的候選提示");
        {
            voicetype::VoiceType addon(&instance, {}, {}, voicetype::VoiceTypeConfig{});
            learn(instance, *client);
            client.reset(); // pending fallback holds only a weak IC reference
        }
    }
    std::filesystem::remove_all(dir);
    return failures ? EXIT_FAILURE : EXIT_SUCCESS;
}
