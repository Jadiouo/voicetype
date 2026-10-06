// A real Fcitx event-loop client and Unix IPC peer reproduce editing a dictation
// then sending/clearing the input BEFORE the next push-to-talk. No user daemon,
// desktop configuration, microphone or persisted learning file is involved.
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
#include <fstream>
#include <sys/stat.h>
#include <vector>

using namespace voicetype;
static int failures = 0;
#define CHECK(condition) do { if (!(condition)) { \
    std::fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #condition); \
    ++failures; } } while (0)

class TextClient : public fcitx::InputContext {
public:
    explicit TextClient(fcitx::Instance &instance)
        : fcitx::InputContext(instance.inputContextManager(), "submit-test") {
        setCapabilityFlags(fcitx::CapabilityFlag::SurroundingText);
    }
    ~TextClient() override { destroy(); }
    const char *frontend() const override { return "voicetype-submit-test"; }
    void setText(const std::string &text, size_t cursor = std::string::npos,
                 size_t anchor = std::string::npos) {
        content = text;
        if (cursor == std::string::npos) { cursor = fcitx::utf8::length(text); }
        if (anchor == std::string::npos) { anchor = cursor; }
        surroundingText().setText(text, cursor, anchor);
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

int main(int argc, char **argv) {
    const std::string scenario = argc > 1 ? argv[1] : "enter";
    const bool selectionCase = scenario.rfind("selection_", 0) == 0;
    const bool asrFailure = scenario.rfind("asr_", 0) == 0;
    const bool scalarDelete = scenario == "backspace_ascii" || scenario == "delete_ascii" ||
                              scenario == "ctrl_backspace" || scenario == "ctrl_delete";
    const bool scalarProbe = scenario == "virtual_ascii" || scenario == "preedit_ascii" ||
                             scenario == "wrong_caret" || scenario == "multikey_ascii" || scenario == "caps_mismatch";
    const bool fast = scenario == "fast_enter" || scenario == "fast_ctrl_enter" ||
                      scenario == "fast_mouse";
    const bool shouldLearn = scenario == "enter" || scenario == "ctrl_enter" ||
        scenario == "mouse" || scenario == "fast_enter" || scenario == "fast_ctrl_enter" ||
        scenario == "keep" || scenario == "anchored" || scenario == "split_replace" ||
        scenario == "manual_paste" || scenario == "backspace_ascii" ||
        scenario == "delete_ascii" || scenario == "caps_ascii";
    char temporary[] = "/tmp/voicetype-submit-XXXXXX";
    const char *dir = mkdtemp(temporary);
    if (!dir) { return EXIT_FAILURE; }
    if (selectionCase) {
        const std::string helper = std::string(dir) + "/selection-helper";
        std::ofstream script(helper);
        script << "#!/usr/bin/python3\nimport json,time\n";
        if (scenario == "selection_timeout") { script << "time.sleep(20)\n"; }
        if (scenario == "selection_focus") { script << "time.sleep(.15)\n"; }
        if (scenario == "selection_error") {
            script << "print(json.dumps({'type':'selection_error','code':'tree_limit'}))\n";
        } else {
            script << "print(json.dumps({'type':'selection','program':'"
                   << (scenario == "selection_wrong_program" ? "other" : "submit-test")
                   << "','text':'我要把這個東西 push 到 GitHub 上面。','context_id':':1.99:/field','value':'0x123:42'}))\n";
        }
        script.close(); chmod(helper.c_str(), 0700);
        setenv("VOICETYPE_SELECTION_HELPER", helper.c_str(), 1);
    }
    const std::string path = std::string(dir) + "/ipc.sock";
    setenv("VOICETYPE_SOCKET", path.c_str(), 1);
    setenv("FCITX_CONFIG_HOME", dir, 1);
    setenv("FCITX_DATA_HOME", dir, 1);
    const int listener = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    sockaddr_un address{};
    address.sun_family = AF_UNIX;
    std::memcpy(address.sun_path, path.c_str(), path.size() + 1);
    CHECK(bind(listener, reinterpret_cast<sockaddr *>(&address), sizeof(address)) == 0);
    CHECK(listen(listener, 1) == 0);
    {
        char name[] = "voicetype-submit-test";
        char disable[] = "--disable=all";
        char *argv[] = {name, disable, nullptr};
        fcitx::Instance instance(2, argv);
        instance.initialize();
        uint64_t clockOffset = 0;
        std::vector<std::pair<std::string, std::string>> notices;
        VoiceType::NotificationSink sink;
        if (scenario != "asr_ui") {
            sink = [&](const std::string &summary, const std::string &body) {
                notices.emplace_back(summary, body);
            };
        }
        VoiceType addon(&instance, std::move(sink),
            [&clockOffset]() { return fcitx::now(CLOCK_MONOTONIC) + clockOffset; },
            VoiceTypeConfig{});
        TextClient client(instance);
        if (selectionCase) { client.setCapabilityFlags(fcitx::CapabilityFlags()); }
        client.focusIn();
        const std::string prefix = scenario == "anchored" ? "原本的筆記：" : "";
        client.setText(prefix);
        if (scenario == "asr_ui") { client.inputPanel().setAuxDown(fcitx::Text("原本提示")); }
        const int peer = accept4(listener, nullptr, nullptr, SOCK_NONBLOCK | SOCK_CLOEXEC);
        CHECK(peer >= 0);
        auto &loop = instance.eventLoop();
        std::vector<IpcMessage> received;
        std::string incoming;
        const std::string before = scenario == "keypress_app_rewrite" || scalarProbe ? "我覺得這個東西 mabe 是對的。" :
            scalarDelete ? "我要把這個東西 push 到 GitHab 上面。" : "我要把這個東西 push 到 gthub 上面。";
        const std::string after = scenario == "keypress_app_rewrite" || scalarProbe ? "我覺得這個東西 maybe 是對的。" : "我要把這個東西 push 到 GitHub 上面。";
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
                    if (message.type == "stop" && message.session == 1) {
                        IpcMessage result;
                        result.type = asrFailure ? "error" : "result";
                        result.setSession(message.session);
                        result.text = asrFailure ? "辨識被拒絕 <b>請重試</b>\n" + std::string(1000, 'x') : before;
                        result.code = scenario == "asr_ui" ? "internal" : scenario.substr(4);
                        std::string wire;
                        if (scenario == "stale_error") {
                            auto stale = result; stale.type = "error"; stale.setSession(0);
                            stale.code = "internal"; stale.text = "舊工作階段的錯誤";
                            wire += serialize(stale) + "\n";
                        }
                        wire += serialize(result) + "\n";
                        if (scenario == "control_error") {
                            auto control = result; control.type = "error"; control.code = "internal";
                            control.text = "詞彙學習已有 daemon 通知";
                            wire += serialize(control) + "\n";
                        }
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
                    key(instance, client, fcitx::Key(FcitxKey_Alt_L, fcitx::KeyState::Ctrl));
                    key(instance, client, fcitx::Key(FcitxKey_Alt_L), true);
                    stage = 1;
                } else if (stage == 1 && asrFailure &&
                    (!notices.empty() || client.inputPanel().auxDown().toString().find("語音輸入未送出") != std::string::npos)) {
                    CHECK(client.commits == 0);
                    CHECK(received.size() == 2); // no correction or other learning request
                    if (scenario == "asr_ui") {
                        CHECK(client.inputPanel().auxDown().toString().find("原本提示\n語音輸入未送出") == 0);
                    } else {
                        CHECK(notices.size() == 1 && notices[0].first == "語音輸入未送出");
                        CHECK(fcitx::utf8::validate(notices[0].second));
                        CHECK(notices[0].second.size() < 1500);
                        CHECK(notices[0].second.find("<b>") == std::string::npos);
                    }
                    loop.exit(); return false;
                } else if (stage == 1 && client.commits == 1 &&
                           (scenario == "stale_error" || scenario == "control_error")) {
                    CHECK(notices.empty());
                    CHECK(received.size() == 2);
                    loop.exit(); return false;
                } else if (stage == 1 && client.commits == 1 && selectionCase) {
                    key(instance, client, fcitx::Key("Control+Caps_Lock"));
                    if (scenario == "selection_success") {
                        key(instance, client, fcitx::Key("Control+Caps_Lock")); // no double request
                    }
                    if (scenario == "selection_focus") { client.focusOut(); client.focusIn(); }
                    editedAt = now;
                    stage = 10;
                } else if (stage == 10 && now - editedAt > 750000) {
                    unsigned corrections = 0;
                    for (const auto &message : received) {
                        if (message.type != "correction") { continue; }
                        ++corrections;
                        CHECK(message.session == 1 && message.confirmed);
                        CHECK(message.before == before && message.after == after);
                    }
                    CHECK(corrections == (scenario == "selection_success" ? 1U : 0U));
                    CHECK(client.commits == 1);
                    if (scenario == "selection_timeout" || scenario == "selection_error") {
                        CHECK(!notices.empty() && notices.back().first == "無法讀取修正，尚未學習");
                    }
                    if (scenario == "selection_wrong_program") {
                        CHECK(!notices.empty() && notices.back().first == "修正未送出");
                    }
                    loop.exit(); return false;
                } else if (stage == 1 && client.commits == 1) {
                    if (scenario == "paste" || scenario == "manual_paste") {
                        key(instance, client, fcitx::Key("Control+v"));
                        client.setText(prefix + after);
                    } else if (scenario == "shift_paste") {
                        key(instance, client, fcitx::Key("Shift+Insert"));
                        client.setText(prefix + after);
                    } else if (scenario == "keypress_app_rewrite") {
                        key(instance, client, fcitx::Key(FcitxKey_x));
                        client.setText(after); // app prevents x and rewrites elsewhere
                    } else if (scenario == "app_only") {
                        client.setText(prefix + after);
                    } else if (scalarDelete) {
                        const auto pos = before.find("GitHab") + 4;
                        const auto cursor = fcitx::utf8::length(before.substr(0,pos));
                        const bool backward = scenario == "backspace_ascii" || scenario == "ctrl_backspace";
                        client.setText(before, cursor + (backward ? 1 : 0));
                        key(instance, client, fcitx::Key(backward ? FcitxKey_BackSpace : FcitxKey_Delete,
                            scenario.rfind("ctrl_",0)==0 ? fcitx::KeyState::Ctrl : fcitx::KeyState::NoState));
                        auto deleted = before; deleted.erase(pos,1); client.setText(deleted,cursor);
                        key(instance, client, fcitx::Key(FcitxKey_u));
                        client.setText(after,cursor+1);
                    } else if (scalarProbe) {
                        const auto cursor = fcitx::utf8::length(before.substr(0,before.find("mabe")))+2;
                        client.setText(before,cursor);
                        if (scenario == "preedit_ascii") { client.inputPanel().setPreedit(fcitx::Text("組字")); }
                        key(instance,client,fcitx::Key(scenario=="caps_mismatch"?FcitxKey_Y:FcitxKey_y,
                            scenario=="virtual_ascii"?fcitx::KeyState::Virtual:scenario=="caps_mismatch"?fcitx::KeyState::CapsLock:fcitx::KeyState::NoState));
                        if (scenario == "multikey_ascii") { key(instance,client,fcitx::Key(FcitxKey_z)); }
                        client.setText(after,scenario=="wrong_caret"?fcitx::utf8::length(after):cursor+1);
                        client.inputPanel().setPreedit(fcitx::Text{});
                    } else {
                        // Real selected ASCII replacement: each literal has its
                        // own key and exact surrounding acknowledgement.
                        auto text = prefix + before;
                        const auto pos = text.find("gthub");
                        const auto cursor = fcitx::utf8::length(text.substr(0, pos));
                        client.setText(text, cursor, cursor + 5);
                        key(instance, client, fcitx::Key(FcitxKey_G, scenario=="caps_ascii"?fcitx::KeyState::CapsLock:fcitx::KeyState::Shift));
                        if (scenario == "split_replace") {
                            auto deleted = text; deleted.erase(pos, 5);
                            client.setText(deleted, cursor);
                        }
                        if (scenario=="split_arbitrary") {
                            auto deleted=text;deleted.erase(pos,5);client.setText(deleted,cursor);
                            client.setText(after,cursor+6);
                            editedAt=now;stage=2;event->setNextInterval(10000);event->setOneShot();return true;
                        }
                        text.replace(pos, 5, "G"); client.setText(text, cursor + 1);
                        const std::string rest = "itHub";
                        for (size_t i=0;i<rest.size();++i) {
                            const auto c = rest[i];
                            key(instance, client, fcitx::Key(static_cast<fcitx::KeySym>(c)));
                            text.insert(pos + i + 1, 1, c);
                            client.setText(text, cursor + i + 2);
                        }
                    }
                    if (scenario == "app_rewrite") {
                        // A second rewrite must not reuse the preceding edit key.
                        client.setText("我要把這個東西 push 到 GitLab 上面。");
                    } else if (scenario == "undo") {
                        key(instance, client, fcitx::Key("Control+z"));
                        client.setText(before);
                    }
                    editedAt = now;
                    stage = 2;
                } else if (stage == 2 && now - editedAt > (fast ? 50000 : 400000)) {
                    if (scenario == "focus") {
                        client.focusOut(); client.focusIn();
                    } else if (scenario == "cancel") {
                        key(instance, client, fcitx::Key(FcitxKey_Escape));
                    } else if (scenario == "delete") {
                        key(instance, client, fcitx::Key("Control+a"));
                        client.setText(after, 0, fcitx::utf8::length(after));
                        key(instance, client, fcitx::Key(FcitxKey_BackSpace));
                    } else if (scenario == "manual_paste") {
                        client.setText(after, 0, fcitx::utf8::length(after));
                        key(instance, client, fcitx::Key("Control+Caps_Lock"));
                    } else if (scenario == "stale") {
                        clockOffset = 301000000;
                    } else if (scenario == "composition") {
                        client.inputPanel().setPreedit(fcitx::Text("組字中"));
                        key(instance, client, fcitx::Key(FcitxKey_Return));
                    } else if (scenario != "mouse" && scenario != "fast_mouse" && scenario != "keep") {
                        key(instance, client, scenario == "ctrl_enter" || scenario == "fast_ctrl_enter"
                            ? fcitx::Key("Control+Return") : fcitx::Key(FcitxKey_Return));
                    }
                    if (scenario == "newline") {
                        client.setText(after + "\n"); // Return can mean a newline, not submission
                    } else if (scenario != "keep") {
                        client.setText(prefix); // send/clear removes only our insertion
                        client.setText(prefix); // repeated updates cannot count twice
                    }
                    key(instance, client, fcitx::Key(FcitxKey_Alt_L, fcitx::KeyState::Ctrl));
                    stage = 3;
                } else if (stage == 3 && !received.empty() && received.back().type == "start" && received.back().session == 2) {
                    unsigned corrections = 0;
                    for (const auto &message : received) {
                        if (message.type != "correction") { continue; }
                        ++corrections;
                        CHECK(message.session == 1);
                        CHECK(message.before == before && message.after == after);
                        CHECK(message.hasConfirmed && message.confirmed == (scenario == "manual_paste"));
                    }
                    if (corrections != (shouldLearn ? 1U : 0U)) {
                        std::fprintf(stderr, "scenario=%s expected=%u corrections=%u\n",
                                     scenario.c_str(), shouldLearn ? 1U : 0U, corrections);
                        ++failures;
                    }
                    loop.exit();
                    return false;
                }
                event->setTime(now + 10000);
                event->setOneShot();
                return true;
            });
        auto timeout = loop.addTimeEvent(CLOCK_MONOTONIC,
            fcitx::now(CLOCK_MONOTONIC) + 3000000, 0,
            [&](fcitx::EventSourceTime *, uint64_t) {
                std::fprintf(stderr, "submit timeout: stage=%d commits=%u messages=%zu\n", stage, client.commits, received.size());
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
