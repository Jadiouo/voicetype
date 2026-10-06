// Real Fcitx event dispatch and Unix IPC, with an in-process text client. No
// desktop frontend, microphone, installed addon, or user configuration is used.
#include "voicetype.h"

#include <fcitx-utils/utf8.h>
#include <fcitx/surroundingtext.h>
#include <fcitx/inputpanel.h>

#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <vector>

using namespace voicetype;
static int failures = 0;
#define CHECK(condition) do { if (!(condition)) { \
    std::fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #condition); \
    ++failures; } } while (0)

class TextClient : public fcitx::InputContext {
public:
    explicit TextClient(fcitx::Instance &instance)
        : fcitx::InputContext(instance.inputContextManager(), "test-editor") {
        setCapabilityFlags(fcitx::CapabilityFlag::SurroundingText);
    }
    ~TextClient() override { destroy(); }
    const char *frontend() const override { return "voicetype-test"; }
    void setText(const std::string &text, size_t anchor = std::string::npos,
                 size_t cursor = std::string::npos) {
        content = text;
        if (cursor == std::string::npos) { cursor = fcitx::utf8::length(text); }
        surroundingText().setText(text, cursor,
                                  anchor == std::string::npos ? cursor : anchor);
        updateSurroundingText();
    }
    std::string content;
    unsigned commits = 0;
protected:
    void commitStringImpl(const std::string &text) override {
        ++commits;
        setText(content + text);
    }
    void deleteSurroundingTextImpl(int, unsigned int) override {}
    void forwardKeyImpl(const fcitx::ForwardKeyEvent &) override {}
    void updatePreeditImpl() override {}
};

static void key(fcitx::Instance &instance, TextClient &client,
                const fcitx::Key &value, bool release = false) {
    fcitx::KeyEvent event(&client, value, release);
    instance.postEvent(event);
}

int main() {
    char temporary[] = "/tmp/voicetype-lifecycle-XXXXXX";
    const char *dir = mkdtemp(temporary);
    if (!dir) { return EXIT_FAILURE; }
    const std::string path = std::string(dir) + "/ipc.sock";
    setenv("VOICETYPE_SOCKET", path.c_str(), 1);
    setenv("VOICETYPE_SELECTION_HELPER", "/nonexistent/voicetype-selection-fixture", 1);
    setenv("FCITX_CONFIG_HOME", dir, 1);
    setenv("FCITX_DATA_HOME", dir, 1);
    const int listener = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    sockaddr_un address{};
    address.sun_family = AF_UNIX;
    std::memcpy(address.sun_path, path.c_str(), path.size() + 1);
    CHECK(bind(listener, reinterpret_cast<sockaddr *>(&address), sizeof(address)) == 0);
    CHECK(listen(listener, 1) == 0);
    {
        char name[] = "voicetype-test";
        char disable[] = "--disable=all";
        char *argv[] = {name, disable, nullptr};
        fcitx::Instance instance(2, argv);
        instance.initialize();
        std::vector<std::string> notices;
        uint64_t clockOffset = 0;
        VoiceType addon(&instance,
            [&notices](const std::string &summary, const std::string &) {
                notices.push_back(summary);
            }, [&clockOffset]() { return fcitx::now(CLOCK_MONOTONIC) + clockOffset; },
            VoiceTypeConfig{});
        TextClient client(instance);
        client.focusIn();
        client.setText("");
        int peer = accept4(listener, nullptr, nullptr, SOCK_NONBLOCK | SOCK_CLOEXEC);
        CHECK(peer >= 0);
        auto &loop = instance.eventLoop();
        std::vector<IpcMessage> received;
        std::string incoming;
        const std::string before = "我要把這個東西 push 到 gthub 上面。";
        const std::string after = "我要把這個東西 push 到 GitHub 上面。";
        auto io = loop.addIOEvent(peer, fcitx::IOEventFlag::In,
            [&](fcitx::EventSourceIO *, int, fcitx::IOEventFlags) {
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
                    received.push_back(message);
                    if (message.type == "stop" && message.session <= 3) {
                        IpcMessage result;
                        result.type = "result";
                        result.setSession(message.session);
                        result.text = before;
                        const auto wire = serialize(result) + "\n";
                        CHECK(write(peer, wire.data(), wire.size()) == static_cast<ssize_t>(wire.size()));
                    }
                }
                return true;
            });
        int stage = 0;
        uint64_t editedAt = 0;
        auto timer = loop.addTimeEvent(CLOCK_MONOTONIC,
            fcitx::now(CLOCK_MONOTONIC) + 1000, 0,
            [&](fcitx::EventSourceTime *event, uint64_t) {
                const auto now = fcitx::now(CLOCK_MONOTONIC);
                if (stage == 0) {
                    key(instance, client, fcitx::Key("Control+Caps_Lock"));
                    CHECK(notices.back() == "沒有可學習的上一句");
                    key(instance, client, fcitx::Key(FcitxKey_Alt_L, fcitx::KeyState::Ctrl));
                    key(instance, client, fcitx::Key("Control+Caps_Lock"));
                    CHECK(notices.back() == "正在錄音，尚未學習");
                    key(instance, client, fcitx::Key(FcitxKey_Alt_L), true);
                    stage = 1;
                } else if (stage == 1 && client.commits == 1) {
                    auto text = before;
                    const auto pos = text.find("gthub");
                    const auto cursor = fcitx::utf8::length(text.substr(0, pos));
                    client.setText(text, cursor + 5, cursor);
                    key(instance, client, fcitx::Key(FcitxKey_G));
                    text.replace(pos, 5, "G"); client.setText(text, cursor + 1, cursor + 1);
                    const std::string rest = "itHub";
                    for (size_t i=0;i<rest.size();++i) {
                        key(instance, client, fcitx::Key(static_cast<fcitx::KeySym>(rest[i])));
                        text.insert(pos + i + 1, 1, rest[i]);
                        client.setText(text, cursor + i + 2, cursor + i + 2);
                    }
                    editedAt = now;
                    stage = 2;
                } else if (stage == 2 && now - editedAt > 400000) {
                    key(instance, client, fcitx::Key(FcitxKey_Alt_L, fcitx::KeyState::Ctrl));
                    stage = 3;
                } else if (stage == 3 && received.size() >= 4) {
                    CHECK(received[0].type == "start");
                    CHECK(received[0].program == "test-editor");
                    CHECK(!received[0].contextId.empty());
                    CHECK(received[1].type == "stop");
                    CHECK(received[2].type == "correction");
                    CHECK(received[2].session == 1);
                    CHECK(received[2].contextId == received[0].contextId);
                    CHECK(received[2].before == before && received[2].after == after);
                    CHECK(received[2].hasConfirmed && !received[2].confirmed);
                    CHECK(received[3].type == "start");
                    CHECK(received[3].contextText == after);
                    key(instance, client, fcitx::Key(FcitxKey_Alt_L), true);
                    stage = 4;
                } else if (stage == 4 && client.commits == 2) {
                    const auto count = received.size();
                    key(instance, client, fcitx::Key("Control+Caps_Lock"));
                    CHECK(notices.back() == "請先選取修正後的完整句子");
                    client.setText(after + before, fcitx::utf8::length(after));
                    key(instance, client, fcitx::Key("Control+Caps_Lock"));
                    CHECK(notices.back() == "選取的句子還沒有修正");
                    client.inputPanel().setPreedit(fcitx::Text("組字中"));
                    key(instance, client, fcitx::Key("Control+Caps_Lock"));
                    CHECK(notices.back() == "請先完成目前的輸入");
                    CHECK(client.inputPanel().preedit().toString() == "組字中");
                    client.inputPanel().setPreedit(fcitx::Text{});
                    client.setCapabilityFlags(fcitx::CapabilityFlags{});
                    key(instance, client, fcitx::Key("Control+Caps_Lock"));
                    CHECK(notices.back() == "選取工具尚未安裝");
                    client.setCapabilityFlags(fcitx::CapabilityFlag::SurroundingText);
                    client.setText(after + std::string(513, 'x'), fcitx::utf8::length(after));
                    key(instance, client, fcitx::Key("Control+Caps_Lock"));
                    CHECK(notices.back() == "選取的文字太長");
                    CHECK(received.size() == count); // rejected attempts send no learning request
                    client.setText(after + after, fcitx::utf8::length(after));
                    key(instance, client, fcitx::Key("Control+Caps_Lock"));
                    CHECK(notices.back() == "已送出修正，等待確認");
                    stage = 5;
                } else if (stage == 5 && received.size() >= 6) {
                    CHECK(received[4].type == "stop" && received[4].session == 2);
                    CHECK(received[5].type == "correction");
                    CHECK(received[5].session == 2 && received[5].confirmed);
                    CHECK(received[5].before == before && received[5].after == after);
                    client.focusOut();
                    client.focusIn();
                    key(instance, client, fcitx::Key("Control+Caps_Lock"));
                    CHECK(notices.back() == "沒有可學習的上一句");
                    client.setText(after + after); // end the previous selection
                    key(instance, client, fcitx::Key(FcitxKey_Alt_L, fcitx::KeyState::Ctrl));
                    key(instance, client, fcitx::Key(FcitxKey_Alt_L), true);
                    stage = 6;
                } else if (stage == 6 && received.size() >= 8 && client.commits == 3) {
                    CHECK(received[6].type == "start");
                    CHECK(received[6].contextId != received[0].contextId);
                    CHECK(received[7].type == "stop");
                    clockOffset = 301000000;
                    key(instance, client, fcitx::Key("Control+Caps_Lock"));
                    CHECK(notices.back() == "上一句已超過學習時限");
                    clockOffset = 0;
                    client.setCapabilityFlags(fcitx::CapabilityFlags(fcitx::CapabilityFlag::SurroundingText) |
                                              fcitx::CapabilityFlag::Sensitive);
                    key(instance, client, fcitx::Key("Control+Caps_Lock"));
                    CHECK(notices.back() == "此欄位無法學習");
                    key(instance, client, fcitx::Key(FcitxKey_Alt_L, fcitx::KeyState::Ctrl));
                    CHECK(notices.back() == "此欄位無法使用語音輸入");
                    key(instance, client, fcitx::Key(FcitxKey_Alt_L), true);
                    stage = 7;
                } else if (stage == 7) {
                    CHECK(received.size() == 8); // sensitive PTT sent no context/audio start
                    io->setEnabled(false);
                    close(peer);
                    peer = -1;
                    client.setCapabilityFlags(fcitx::CapabilityFlag::SurroundingText);
                    editedAt = now;
                    stage = 8;
                } else if (stage == 8 && now - editedAt > 100000) {
                    key(instance, client, fcitx::Key("Control+Caps_Lock"));
                    CHECK(notices.back() == "語音服務未連線");
                    loop.exit();
                    return false;
                }
                event->setTime(now + 10000);
                event->setOneShot();
                return true;
            });
        auto timeout = loop.addTimeEvent(CLOCK_MONOTONIC,
            fcitx::now(CLOCK_MONOTONIC) + 5000000, 0,
            [&](fcitx::EventSourceTime *, uint64_t) {
                std::fprintf(stderr, "lifecycle timeout: stage=%d commits=%u messages=%zu\n",
                             stage, client.commits, received.size());
                for (const auto &message : received) {
                    std::fprintf(stderr, "%s\n", serialize(message).c_str());
                }
                ++failures;
                loop.exit();
                return false;
            });
        loop.exec();
        if (peer >= 0) { close(peer); }
    }
    close(listener);
    std::filesystem::remove_all(dir);
    return failures ? EXIT_FAILURE : EXIT_SUCCESS;
}
