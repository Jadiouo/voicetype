#include "selection.h"

#include <algorithm>
#include <cerrno>
#include <csignal>
#include <fcntl.h>
#include <spawn.h>
#include <sys/wait.h>
#include <thread>
#include <unistd.h>

extern char **environ;

namespace voicetype {

SelectionReader::~SelectionReader() {
    cancel();
    timer_.reset();
    if (pid_ > 0) {
        const auto child = pid_;
        int status;
        if (waitpid(child, &status, WNOHANG) == 0) {
            // Destruction must not block the input-method event loop either.
            // The already-killed child is reaped independently; no UI captures.
            std::thread([child]() {
                int result;
                while (waitpid(child, &result, 0) < 0 && errno == EINTR) {}
            }).detach();
        }
    }
}

bool SelectionReader::start(const std::string &helper, const std::string &program,
                            Callback callback) {
    if (busy() || access(helper.c_str(), X_OK) != 0) { return false; }
    int pipefd[2];
    if (pipe2(pipefd, O_CLOEXEC) < 0) { return false; }
    int flags;
    do { flags = fcntl(pipefd[0], F_GETFL); } while (flags < 0 && errno == EINTR);
    int nonblock = -1;
    if (flags >= 0) {
        do { nonblock = fcntl(pipefd[0], F_SETFL, flags | O_NONBLOCK); }
        while (nonblock < 0 && errno == EINTR);
    }
    if (nonblock < 0) { close(pipefd[0]); close(pipefd[1]); return false; }
    posix_spawn_file_actions_t actions;
    posix_spawn_file_actions_init(&actions);
    posix_spawn_file_actions_addopen(&actions, STDIN_FILENO, "/dev/null", O_RDONLY, 0);
    posix_spawn_file_actions_addopen(&actions, STDERR_FILENO, "/dev/null", O_WRONLY, 0);
    posix_spawn_file_actions_adddup2(&actions, pipefd[1], STDOUT_FILENO);
    posix_spawn_file_actions_addclose(&actions, pipefd[0]);
    posix_spawn_file_actions_addclose(&actions, pipefd[1]);
    posix_spawnattr_t attr;
    posix_spawnattr_init(&attr);
    posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETPGROUP);
    posix_spawnattr_setpgroup(&attr, 0);
    char *argv[] = {const_cast<char *>(helper.c_str()), const_cast<char *>("--program"),
                    const_cast<char *>(program.c_str()), nullptr};
    pid_t child = -1;
    const auto error = posix_spawn(&child, helper.c_str(), &actions, &attr, argv, environ);
    posix_spawn_file_actions_destroy(&actions);
    posix_spawnattr_destroy(&attr);
    close(pipefd[1]);
    if (error) { close(pipefd[0]); return false; }
    pid_ = child;
    fd_ = pipefd[0];
    bytes_.clear();
    delivered_ = false;
    callback_ = std::move(callback);
    deadline_ = fcitx::now(CLOCK_MONOTONIC) + 500000;
    const auto next = fcitx::now(CLOCK_MONOTONIC) + 1000;
    if (!timer_) {
        timer_ = loop_->addTimeEvent(CLOCK_MONOTONIC, next, 1,
            [this](fcitx::EventSourceTime *event, uint64_t) {
                tick();
                if (pid_ <= 0) { return false; }
                const auto next = fcitx::now(CLOCK_MONOTONIC) + 5000;
                event->setTime(delivered_ ? next : std::min(next, deadline_));
                event->setOneShot();
                return true;
            });
    } else {
        timer_->setTime(next);
        timer_->setOneShot();
    }
    return true;
}

void SelectionReader::closePipe() {
    if (fd_ >= 0) { close(fd_); fd_ = -1; }
}

void SelectionReader::cancel() {
    callback_ = {};
    delivered_ = true;
    bytes_.clear();
    closePipe();
    if (pid_ > 0) { kill(-pid_, SIGKILL); }
    // Keep the timer solely for nonblocking waitpid/reaping.
}

void SelectionReader::complete(IpcMessage result) {
    if (delivered_) { return; }
    delivered_ = true;
    bytes_.clear();
    auto callback = std::move(callback_);
    if (callback) { callback(result); }
}

void SelectionReader::fail(const char *code) {
    if (pid_ > 0) { kill(-pid_, SIGKILL); }
    closePipe();
    IpcMessage error;
    error.type = "selection_error";
    error.code = code;
    complete(std::move(error));
}

void SelectionReader::drain() {
    if (fd_ >= 0) {
        char buffer[2048];
        for (;;) {
            const auto count = read(fd_, buffer, sizeof(buffer));
            if (count > 0) {
                bytes_.append(buffer, static_cast<size_t>(count));
                if (bytes_.size() > 8192) { fail("output_too_large"); break; }
            } else if (count == 0) { closePipe(); break; }
            else if (errno == EINTR) { continue; }
            else if (errno == EAGAIN || errno == EWOULDBLOCK) { break; }
            else { fail("helper_failed"); break; }
        }
    }
}

void SelectionReader::tick() {
    if (pid_ <= 0) { return; }
    drain();
    int status = 0;
    const auto waited = waitpid(pid_, &status, WNOHANG);
    if (waited == pid_ || (waited < 0 && errno == ECHILD)) {
        kill(-pid_, SIGKILL); // clean any helper descendants retaining the pipe
        pid_ = -1;
        // The child can write between the first EAGAIN and waitpid. Drain the
        // final bytes after exit before parsing, including fast write+exit.
        drain();
        closePipe();
        if (!delivered_) {
            IpcMessage result;
            if (waited < 0 || !WIFEXITED(status) || WEXITSTATUS(status) != 0 ||
                !parse(bytes_, result) || (result.type != "selection" && result.type != "selection_error")) {
                result = {};
                result.type = "selection_error";
                result.code = "helper_failed";
            }
            complete(std::move(result));
        }
    } else if (!delivered_ && fcitx::now(CLOCK_MONOTONIC) >= deadline_) {
        fail("timeout");
    }
}

} // namespace voicetype
