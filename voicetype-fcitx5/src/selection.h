#ifndef VOICETYPE_SELECTION_H
#define VOICETYPE_SELECTION_H

#include "ipc.h"
#include <sys/types.h>

namespace voicetype {

// A bounded, explicit-action subprocess. Never runs on ordinary dictation.
class SelectionReader {
public:
    using Callback = std::function<void(const IpcMessage &)>;
    explicit SelectionReader(fcitx::EventLoop *loop) : loop_(loop) {}
    ~SelectionReader();
    bool start(const std::string &helper, const std::string &program, Callback callback);
    void cancel();
    bool busy() const { return pid_ > 0; }
private:
    void tick();
    void drain();
    void closePipe();
    void fail(const char *code);
    void complete(IpcMessage result);
    fcitx::EventLoop *loop_;
    pid_t pid_ = -1;
    int fd_ = -1;
    uint64_t deadline_ = 0;
    bool delivered_ = false;
    std::string bytes_;
    Callback callback_;
    std::unique_ptr<fcitx::EventSourceTime> timer_;
};

} // namespace voicetype
#endif
