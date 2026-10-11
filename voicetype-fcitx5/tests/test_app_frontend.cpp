// OS input-context boundary for the Rust worker integration test. Loads the
// actual installed module from a disposable Fcitx data directory.
#include <fcitx/instance.h>
#include <fcitx/addonmanager.h>
#include <fcitx/inputcontext.h>
#include <fcitx/focusgroup.h>
#include <fcitx/event.h>
#include <fcitx-utils/event.h>
#include <cstdlib>
#include <fstream>
#include <string>

class Client : public fcitx::InputContext {
public:
    Client(fcitx::Instance &instance, const std::string &root)
        : InputContext(instance.inputContextManager(), "app-test"), root_(root) { created(); }
    ~Client() override { destroy(); }
    const char *frontend() const override { return "app-test"; }
protected:
    void commitStringImpl(const std::string &text) override { std::ofstream(root_ + "/commits", std::ios::app) << text << '\n'; }
    void deleteSurroundingTextImpl(int, unsigned int) override {}
    void forwardKeyImpl(const fcitx::ForwardKeyEvent &) override {}
    void updatePreeditImpl() override {}
private:
    std::string root_;
};

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    const std::string root = argv[1];
    setenv("HOME", root.c_str(), 1);
    setenv("XDG_RUNTIME_DIR", (root + "/runtime").c_str(), 1);
    setenv("FCITX_CONFIG_HOME", (root + "/fcitx-config").c_str(), 1);
    setenv("FCITX_DATA_HOME", (root + "/fcitx").c_str(), 1);
    setenv("VOICETYPE_SOCKET", (root + "/absent-legacy.sock").c_str(), 1);
    unsetenv("DISPLAY"); unsetenv("WAYLAND_DISPLAY");
    setenv("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent-voicetype-test-bus", 1);
    char name[] = "app-frontend-test", disabled[] = "--disable=all", enabled[] = "--enable=voicetype";
    char *args[] = {name, disabled, enabled, nullptr};
    fcitx::Instance instance(3, args);
    instance.addonManager().registerDefaultLoader(nullptr);
    instance.initialize();
    if (!instance.addonManager().addon("voicetype", true)) return 3;
    fcitx::FocusGroup group("app-test:main", instance.inputContextManager());
    Client client(instance, root);
    client.setFocusGroup(&group); client.focusIn();
    uint64_t started = fcitx::now(CLOCK_MONOTONIC);
    std::string previous;
    bool finished = false;
    auto timer = instance.eventLoop().addTimeEvent(CLOCK_MONOTONIC, started, 0,
        [&](fcitx::EventSourceTime *source, uint64_t now) {
            std::ifstream control(root + "/control");
            std::string command; std::getline(control, command);
            if (command != previous) {
                previous = command;
                if (command == "start" || command == "stop") {
                    fcitx::KeyEvent event(&client, fcitx::Key(FcitxKey_Alt_L, fcitx::KeyState::Ctrl), command == "stop");
                    instance.postEvent(event);
                } else if (command == "finish") { finished = true; instance.exit(); return false; }
            }
            if (now - started > 10000000) { instance.exit(); return false; }
            source->setTime(now + 10000); source->setOneShot(); return true;
        });
    instance.exec();
    return finished ? 0 : 4;
}
