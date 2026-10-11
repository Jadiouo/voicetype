#include "input_route.h"
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <string>
#include <sys/stat.h>
#include <unistd.h>

int main() {
    const auto temp = std::getenv("TMPDIR");
    if (!temp || temp[0] != '/') return 2;
    auto pattern = std::string(temp) + "/google-route-XXXXXX";
    const auto root = mkdtemp(pattern.data());
    if (!root) return 3;
    const auto directory = std::filesystem::path(root) / "voicetype-app-input";
    std::filesystem::create_directory(directory);
    chmod(directory.c_str(), 0700);
    setenv("XDG_RUNTIME_DIR", root, 1);
    const auto owner = directory / "owner";
    auto write = [&](const std::string &data) {
        std::ofstream(owner, std::ios::binary | std::ios::trunc) << data;
        chmod(owner.c_str(), 0600);
    };
    const auto pid = std::to_string(getpid());
    write("voicetype-input-v2\n" + pid + "\n/owned/input.sock\ngoogle-toggle\n");
    const auto google = voicetype::desktopInputRoute();
    if (!google || !google->googleToggle || google->socket != "/owned/input.sock" || google->pid != getpid()) return 4;
    write("voicetype-input-v2\n" + pid + "\n/owned/input.sock\nunsupported\n");
    if (voicetype::desktopInputRoute()) return 5;
    write("voicetype-input-v1\n" + pid + "\n/owned/input.sock\n");
    const auto local = voicetype::desktopInputRoute();
    if (!local || local->googleToggle) return 6;
    using A = voicetype::GoogleToggleAction;
    using voicetype::googleToggleAction;
    // A complete toggle session: only the first physical key-down starts,
    // only the next one in the original target stops, and pending delivery
    // cannot become a third capture.
    if (googleToggleAction(false, false, false, false, true) != A::None ||
        googleToggleAction(true, false, false, false, true) != A::Start ||
        googleToggleAction(true, true, true, true, true) != A::None ||
        googleToggleAction(false, false, true, true, true) != A::None ||
        googleToggleAction(true, false, true, true, false) != A::None ||
        googleToggleAction(true, false, true, true, true) != A::Stop ||
        googleToggleAction(true, false, false, true, true) != A::None ||
        googleToggleAction(true, false, false, false, true) != A::Start) return 7;
    std::filesystem::remove_all(root);
    return 0;
}
