#ifndef VOICETYPE_INPUT_ROUTE_H
#define VOICETYPE_INPUT_ROUTE_H
#include <optional>
#include <string>
namespace voicetype {
struct InputRoute { std::string socket; int pid; };
// An explicit app lease in this login's private runtime directory. Invalid,
// missing or untrusted records mean the unchanged legacy endpoint.
std::optional<InputRoute> desktopInputRoute();
}
#endif
