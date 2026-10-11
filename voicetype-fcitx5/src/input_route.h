#ifndef VOICETYPE_INPUT_ROUTE_H
#define VOICETYPE_INPUT_ROUTE_H
#include <optional>
#include <string>
namespace voicetype {
struct InputRoute {
    std::string socket;
    int pid;
    bool googleToggle = false;
    bool ctrlShiftF9 = false;
};
enum class GoogleToggleAction { None, Start, Stop };
// A physical key-down toggles only after the prior delivery finishes. Repeat,
// key-up and a different target cannot create or stop a session.
GoogleToggleAction googleToggleAction(bool keyDown, bool repeat, bool recording,
                                     bool deliveryPending, bool originalTarget);
// An explicit app lease in this login's private runtime directory. Invalid,
// missing or untrusted records mean the unchanged legacy endpoint.
std::optional<InputRoute> desktopInputRoute();
}
#endif
