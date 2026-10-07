// Public Fcitx events + actual sockets; no user's input frontend or microphone.
#include "voicetype.h"
#include <fcitx/focusgroup.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <vector>

using namespace voicetype;
static int failures = 0;
#define CHECK(x) do { if (!(x)) { fprintf(stderr, "FAIL %d: %s\n", __LINE__, #x); ++failures; } } while (0)

class Client : public fcitx::InputContext {
public:
    explicit Client(fcitx::Instance &instance)
        : InputContext(instance.inputContextManager(), "handoff-test") { created(); }
    ~Client() override { destroy(); }
    const char *frontend() const override { return "handoff-test"; }
    std::vector<std::string> commits;
protected:
    void commitStringImpl(const std::string &text) override { commits.push_back(text); }
    void deleteSurroundingTextImpl(int, unsigned int) override {}
    void forwardKeyImpl(const fcitx::ForwardKeyEvent &) override {}
    void updatePreeditImpl() override {}
};

static int listenAt(const std::string &path) {
    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
    sockaddr_un address{};
    address.sun_family = AF_UNIX;
    std::memcpy(address.sun_path, path.c_str(), path.size() + 1);
    CHECK(bind(fd, reinterpret_cast<sockaddr *>(&address), sizeof(address)) == 0);
    CHECK(listen(fd, 4) == 0);
    return fd;
}
static void sendMessage(int fd, IpcMessage message) {
    const auto wire = serialize(message) + "\n";
    CHECK(send(fd, wire.data(), wire.size(), MSG_NOSIGNAL) == static_cast<ssize_t>(wire.size()));
}
static void key(fcitx::Instance &instance, Client &client, bool release) {
    fcitx::KeyEvent event(&client, fcitx::Key(FcitxKey_Alt_L, fcitx::KeyState::Ctrl), release);
    instance.postEvent(event);
}

int main(int argc, char **argv) {
    const std::string scenario = argc > 1 ? argv[1] : "crash";
    char temporary[] = "/tmp/voicetype-handoff-XXXXXX";
    const char *root = mkdtemp(temporary);
    if (!root) return EXIT_FAILURE;
    const std::string legacy = std::string(root) + "/legacy.sock";
    const std::string app = std::string(root) + "/frontend.sock";
    const std::string routeDir = std::string(root) + "/voicetype-app-input";
    setenv("XDG_RUNTIME_DIR", root, 1);
    setenv("VOICETYPE_SOCKET", legacy.c_str(), 1);
    setenv("FCITX_CONFIG_HOME", root, 1);
    setenv("FCITX_DATA_HOME", root, 1);
    unsetenv("DISPLAY"); unsetenv("WAYLAND_DISPLAY"); unsetenv("DBUS_SESSION_BUS_ADDRESS");
    int oldListener = listenAt(legacy), appListener = listenAt(app);
    {
        char name[] = "handoff-test", disable[] = "--disable=all";
        char *args[] = {name, disable, nullptr};
        fcitx::Instance instance(2, args);
        instance.initialize();
        VoiceType addon(&instance, [](const std::string &, const std::string &) {}, {}, VoiceTypeConfig{});
        fcitx::FocusGroup group("handoff-test:main", instance.inputContextManager());
        Client client(instance);
        client.setFocusGroup(&group); client.focusIn();
        int oldPeer = accept4(oldListener, nullptr, nullptr, SOCK_NONBLOCK | SOCK_CLOEXEC);
        CHECK(oldPeer >= 0);
        int appPeer = -1, fallbackPeer = -1, stage = 0;
        int oldStarts = 0, appStarts = 0, appStops = 0, delivered = 0, fallbackStarts = 0;
        std::string oldBuffer, appBuffer, fallbackBuffer;
        IpcMessage oldStart, appStart;
        uint64_t started = fcitx::now(CLOCK_MONOTONIC), published = 0;
        auto drain = [](int fd, std::string &buffer, auto callback) {
            if (fd < 0) return;
            char bytes[4096]; ssize_t size;
            while ((size = read(fd, bytes, sizeof(bytes))) > 0) buffer.append(bytes, size);
            size_t end;
            while ((end = buffer.find('\n')) != std::string::npos) {
                IpcMessage message;
                CHECK(parse(buffer.substr(0, end), message));
                buffer.erase(0, end + 1); callback(message);
            }
        };
        auto timer = instance.eventLoop().addTimeEvent(CLOCK_MONOTONIC, started, 0,
            [&](fcitx::EventSourceTime *source, uint64_t now) {
                drain(oldPeer, oldBuffer, [&](const IpcMessage &m) {
                    if (m.type == "start") { ++oldStarts; oldStart = m; }
                });
                drain(appPeer, appBuffer, [&](const IpcMessage &m) {
                    if (m.type == "desktop_hello") { CHECK(m.value == "voicetype.fcitx.v1"); key(instance, client, false); key(instance, client, true); }
                    if (m.type == "start") { ++appStarts; appStart = m; }
                    if (m.type == "stop") ++appStops;
                    if (m.type == "delivered" && m.code == "committed") ++delivered;
                });
                drain(fallbackPeer, fallbackBuffer, [&](const IpcMessage &m) { if (m.type == "start") ++fallbackStarts; });
                if (stage == 0) { key(instance, client, false); stage = 1; }
                else if (stage == 1 && oldStarts == 1) {
                    CHECK(mkdir(routeDir.c_str(), 0700) == 0);
                    std::ofstream file(routeDir + "/owner");
                    file << "voicetype-input-v1\n" << (scenario == "pid_mismatch" ? getpid() + 1 : getpid()) << "\n" << app << "\n"; file.close();
                    CHECK(chmod((routeDir + "/owner").c_str(), 0600) == 0);
                    published = now; stage = 2;
                } else if (stage == 2 && now - published > 350000) {
                    CHECK(accept4(appListener, nullptr, nullptr, SOCK_NONBLOCK | SOCK_CLOEXEC) == -1);
                    key(instance, client, true);
                    published = now; stage = 20;
                } else if (stage == 20 && now - published > 350000) {
                    // Released key does not mean the pending result may be cut off.
                    CHECK(accept4(appListener, nullptr, nullptr, SOCK_NONBLOCK | SOCK_CLOEXEC) == -1);
                    IpcMessage result; result.type = "result"; result.setSession(oldStart.session); result.text = "原服務 GitHub";
                    sendMessage(oldPeer, result); stage = 3;
                } else if (stage == 3) {
                    appPeer = accept4(appListener, nullptr, nullptr, SOCK_NONBLOCK | SOCK_CLOEXEC);
                    if (appPeer >= 0) {
                        CHECK(client.commits == std::vector<std::string>{"原服務 GitHub"});
                        if (scenario == "pid_mismatch") {
                            stage = 6;
                        } else if (scenario == "handshake_timeout") {
                            // A connected but unready app must not receive recording keys.
                            key(instance, client, false); key(instance, client, true);
                            stage = 6;
                        } else {
                            IpcMessage hello; hello.type = "desktop_hello"; hello.setSession(987); sendMessage(appPeer, hello); stage = 4;
                        }
                    }
                } else if (stage == 4 && appStops == 1) {
                    IpcMessage result; result.type = "deliver"; result.setSession(appStart.session);
                    result.contextId = appStart.contextId; result.text = "請 commit 到 GitHub，保留 Antigravity。";
                    sendMessage(appPeer, result); sendMessage(appPeer, result); stage = 5;
                } else if (stage == 5 && client.commits.size() == 2) {
                    CHECK(appStarts == 1);
                    CHECK(client.commits.back() == "請 commit 到 GitHub，保留 Antigravity。");
                    // Crash: stale record remains. Fcitx must restore legacy without a new app.
                    close(appPeer); appPeer = -1; close(appListener); appListener = -1; stage = 6;
                } else if (stage == 6) {
                    fallbackPeer = accept4(oldListener, nullptr, nullptr, SOCK_NONBLOCK | SOCK_CLOEXEC);
                    if (fallbackPeer >= 0) { key(instance, client, false); key(instance, client, true); stage = 7; }
                } else if (stage == 7 && fallbackStarts == 1) {
                    CHECK(oldStarts == 1);
                    CHECK(appStarts == (scenario == "crash" ? 1 : 0));
                    CHECK(delivered == (scenario == "crash" ? 1 : 0));
                    instance.exit(); return false;
                }
                if (now - started > 3000000) { fprintf(stderr, "handoff timed out at stage %d\n", stage); ++failures; instance.exit(); return false; }
                source->setTime(now + 10000); source->setOneShot(); return true;
            });
        instance.exec();
        if (oldPeer >= 0) close(oldPeer);
        if (appPeer >= 0) close(appPeer);
        if (fallbackPeer >= 0) close(fallbackPeer);
    }
    close(oldListener); if (appListener >= 0) close(appListener);
    std::filesystem::remove_all(root);
    return failures ? EXIT_FAILURE : EXIT_SUCCESS;
}
