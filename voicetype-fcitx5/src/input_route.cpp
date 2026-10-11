#include "input_route.h"
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>
#include <charconv>
#include <cstdlib>

namespace voicetype {
GoogleToggleAction googleToggleAction(bool keyDown, bool repeat, bool recording,
                                     bool deliveryPending, bool originalTarget) {
    if (!keyDown || repeat) return GoogleToggleAction::None;
    if (recording) return originalTarget ? GoogleToggleAction::Stop : GoogleToggleAction::None;
    return deliveryPending ? GoogleToggleAction::None : GoogleToggleAction::Start;
}
namespace {
struct Fd {
    int value;
    ~Fd() { if (value >= 0) close(value); }
};
bool privateFile(int fd, bool directory) {
    struct stat info{};
    return fd >= 0 && fstat(fd, &info) == 0 && info.st_uid == geteuid() &&
        (info.st_mode & 0077) == 0 && (directory ? S_ISDIR(info.st_mode) : S_ISREG(info.st_mode));
}
}

std::optional<InputRoute> desktopInputRoute() {
    const char *runtime = getenv("XDG_RUNTIME_DIR");
    if (!runtime || runtime[0] != '/') return std::nullopt;
    Fd root{open(runtime, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)};
    if (!privateFile(root.value, true)) return std::nullopt;
    Fd directory{openat(root.value, "voicetype-app-input", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)};
    if (!privateFile(directory.value, true)) return std::nullopt;
    Fd record{openat(directory.value, "owner", O_RDONLY | O_NONBLOCK | O_NOFOLLOW | O_CLOEXEC)};
    if (!privateFile(record.value, false)) return std::nullopt;
    char bytes[256];
    const auto length = read(record.value, bytes, sizeof(bytes));
    if (length <= 0 || length == sizeof(bytes)) return std::nullopt;
    const std::string data(bytes, length);
    const bool alternate = data.rfind("voicetype-input-v3\n", 0) == 0;
    const bool v2Google = data.rfind("voicetype-input-v2\n", 0) == 0;
    const std::string prefix = alternate ? "voicetype-input-v3\n" :
        (v2Google ? "voicetype-input-v2\n" : "voicetype-input-v1\n");
    if (data.rfind(prefix, 0) != 0) return std::nullopt;
    const auto end = data.find('\n', prefix.size());
    if (end == std::string::npos) return std::nullopt;
    int pid = 0;
    const auto parsed = std::from_chars(data.data() + prefix.size(), data.data() + end, pid);
    if (parsed.ec != std::errc{} || parsed.ptr != data.data() + end || pid <= 0) return std::nullopt;
    const auto suffix = data.substr(end + 1);
    const auto socketEnd = suffix.find('\n');
    if (socketEnd == std::string::npos) return std::nullopt;
    const auto socket = suffix.substr(0, socketEnd);
    const auto mode = suffix.substr(socketEnd + 1);
    const bool googleToggle = alternate ? mode == "google-toggle-ctrl-shift-f9\n" : v2Google;
    if ((alternate ? (mode != "google-toggle-ctrl-shift-f9\n" &&
                       mode != "local-hold-ctrl-shift-f9\n") :
                     mode != (googleToggle ? "google-toggle\n" : "")) ||
        socket.empty() || socket[0] != '/' || socket.size() >= 108 ||
        socket.find_first_of("\n\r") != std::string::npos || socket.find('\0') != std::string::npos) return std::nullopt;
    return InputRoute{socket, pid, googleToggle, alternate};
}
}
