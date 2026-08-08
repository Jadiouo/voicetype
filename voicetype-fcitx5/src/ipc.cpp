#include "ipc.h"

#include <fcitx-utils/log.h>

#include <sys/socket.h>
#include <sys/types.h>
#include <sys/un.h>
#include <unistd.h>

#include <cerrno>
#include <cstdlib>
#include <cstring>

namespace voicetype {

// ---------------------------------------------------------------------------
// NDJSON
//
// 協定只有九種訊息、全部是扁平物件、值只有 string / number / bool
// (SDD §4.3), 因此手寫編解碼比引入 JSON 函式庫划算——addon 要保持極薄
// 且零額外依賴。唯一需要認真對待的是字串跳脫: text 欄位含使用者的
// 中文轉錄內容, 解錯就是亂碼進到編輯器。
// ---------------------------------------------------------------------------

namespace {

void appendEscaped(std::string &out, const std::string &s) {
    out += '"';
    for (unsigned char c : s) {
        switch (c) {
        case '"':
            out += "\\\"";
            break;
        case '\\':
            out += "\\\\";
            break;
        case '\n':
            out += "\\n";
            break;
        case '\r':
            out += "\\r";
            break;
        case '\t':
            out += "\\t";
            break;
        case '\b':
            out += "\\b";
            break;
        case '\f':
            out += "\\f";
            break;
        default:
            if (c < 0x20) {
                char buf[7];
                snprintf(buf, sizeof(buf), "\\u%04x", c);
                out += buf;
            } else {
                // UTF-8 位元組原樣輸出。JSON 允許, 且比 \u 跳脫短。
                out += static_cast<char>(c);
            }
        }
    }
    out += '"';
}

void appendUtf8(std::string &out, uint32_t cp) {
    if (cp < 0x80) {
        out += static_cast<char>(cp);
    } else if (cp < 0x800) {
        out += static_cast<char>(0xC0 | (cp >> 6));
        out += static_cast<char>(0x80 | (cp & 0x3F));
    } else if (cp < 0x10000) {
        out += static_cast<char>(0xE0 | (cp >> 12));
        out += static_cast<char>(0x80 | ((cp >> 6) & 0x3F));
        out += static_cast<char>(0x80 | (cp & 0x3F));
    } else {
        out += static_cast<char>(0xF0 | (cp >> 18));
        out += static_cast<char>(0x80 | ((cp >> 12) & 0x3F));
        out += static_cast<char>(0x80 | ((cp >> 6) & 0x3F));
        out += static_cast<char>(0x80 | (cp & 0x3F));
    }
}

void skipSpace(const std::string &s, size_t &i) {
    while (i < s.size() &&
           (s[i] == ' ' || s[i] == '\t' || s[i] == '\r' || s[i] == '\n')) {
        ++i;
    }
}

bool parseHex4(const std::string &s, size_t &i, uint32_t &out) {
    if (i + 4 > s.size()) {
        return false;
    }
    uint32_t v = 0;
    for (int k = 0; k < 4; ++k) {
        char c = s[i + k];
        v <<= 4;
        if (c >= '0' && c <= '9') {
            v |= static_cast<uint32_t>(c - '0');
        } else if (c >= 'a' && c <= 'f') {
            v |= static_cast<uint32_t>(c - 'a' + 10);
        } else if (c >= 'A' && c <= 'F') {
            v |= static_cast<uint32_t>(c - 'A' + 10);
        } else {
            return false;
        }
    }
    i += 4;
    out = v;
    return true;
}

bool parseString(const std::string &s, size_t &i, std::string &out) {
    if (i >= s.size() || s[i] != '"') {
        return false;
    }
    ++i;
    out.clear();
    while (i < s.size()) {
        char c = s[i];
        if (c == '"') {
            ++i;
            return true;
        }
        if (c != '\\') {
            out += c;
            ++i;
            continue;
        }
        // 跳脫序列
        ++i;
        if (i >= s.size()) {
            return false;
        }
        char e = s[i++];
        switch (e) {
        case '"':
            out += '"';
            break;
        case '\\':
            out += '\\';
            break;
        case '/':
            out += '/';
            break;
        case 'n':
            out += '\n';
            break;
        case 'r':
            out += '\r';
            break;
        case 't':
            out += '\t';
            break;
        case 'b':
            out += '\b';
            break;
        case 'f':
            out += '\f';
            break;
        case 'u': {
            uint32_t cp = 0;
            if (!parseHex4(s, i, cp)) {
                return false;
            }
            // Surrogate pair。serde_json 預設不跳脫非 ASCII, 所以這條路徑
            // 平常不會走到, 但協定不該假設對端的實作細節。
            if (cp >= 0xD800 && cp <= 0xDBFF) {
                if (i + 1 < s.size() && s[i] == '\\' && s[i + 1] == 'u') {
                    size_t save = i;
                    i += 2;
                    uint32_t lo = 0;
                    if (parseHex4(s, i, lo) && lo >= 0xDC00 && lo <= 0xDFFF) {
                        cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                    } else {
                        i = save;
                        cp = 0xFFFD;
                    }
                } else {
                    cp = 0xFFFD;
                }
            } else if (cp >= 0xDC00 && cp <= 0xDFFF) {
                // 落單的 low surrogate
                cp = 0xFFFD;
            }
            appendUtf8(out, cp);
            break;
        }
        default:
            return false;
        }
    }
    return false; // 字串未終結
}

// 略過一個值並選擇性取出。只支援本協定用到的型別。
bool parseValue(const std::string &s, size_t &i, std::string &strOut,
                uint64_t &numOut, bool &boolOut, int &kind) {
    skipSpace(s, i);
    if (i >= s.size()) {
        return false;
    }
    char c = s[i];
    if (c == '"') {
        kind = 0;
        return parseString(s, i, strOut);
    }
    if (c == 't' || c == 'f') {
        kind = 2;
        if (s.compare(i, 4, "true") == 0) {
            boolOut = true;
            i += 4;
            return true;
        }
        if (s.compare(i, 5, "false") == 0) {
            boolOut = false;
            i += 5;
            return true;
        }
        return false;
    }
    if (c == 'n') {
        kind = 3;
        if (s.compare(i, 4, "null") == 0) {
            i += 4;
            return true;
        }
        return false;
    }
    if ((c >= '0' && c <= '9') || c == '-') {
        kind = 1;
        size_t start = i;
        if (c == '-') {
            ++i;
        }
        uint64_t v = 0;
        bool any = false;
        while (i < s.size() && s[i] >= '0' && s[i] <= '9') {
            v = v * 10 + static_cast<uint64_t>(s[i] - '0');
            ++i;
            any = true;
        }
        // 小數/指數部分本協定不用, 但為了不卡住解析仍需消耗掉
        while (i < s.size() && (s[i] == '.' || s[i] == 'e' || s[i] == 'E' ||
                                s[i] == '+' || s[i] == '-' ||
                                (s[i] >= '0' && s[i] <= '9'))) {
            ++i;
        }
        if (!any) {
            i = start;
            return false;
        }
        numOut = v;
        return true;
    }
    // 巢狀物件/陣列不在協定內, 視為錯誤而非嘗試略過。
    return false;
}

} // namespace

std::string serialize(const IpcMessage &msg) {
    std::string out;
    out.reserve(64 + msg.text.size());
    out += "{\"type\":";
    appendEscaped(out, msg.type);
    if (msg.hasSession) {
        out += ",\"session\":";
        out += std::to_string(msg.session);
    }
    if (!msg.program.empty()) {
        out += ",\"program\":";
        appendEscaped(out, msg.program);
    }
    if (msg.hasIsPassword) {
        out += ",\"is_password\":";
        out += msg.isPassword ? "true" : "false";
    }
    if (!msg.text.empty()) {
        out += ",\"text\":";
        appendEscaped(out, msg.text);
    }
    if (!msg.code.empty()) {
        out += ",\"code\":";
        appendEscaped(out, msg.code);
    }
    if (!msg.value.empty()) {
        out += ",\"value\":";
        appendEscaped(out, msg.value);
    }
    out += "}";
    return out;
}

bool parse(const std::string &line, IpcMessage &out) {
    out = IpcMessage{};

    size_t i = 0;
    skipSpace(line, i);
    if (i >= line.size() || line[i] != '{') {
        return false;
    }
    ++i;
    skipSpace(line, i);
    if (i < line.size() && line[i] == '}') {
        return false; // 空物件沒有 type, 無意義
    }

    // 必須真的看到結尾的 `}`。少了這個旗標, 被截斷的訊息 (例如
    // `{"type":"result",`) 會因為迴圈自然結束而被當成合法訊息 ——
    // NDJSON 的一行有可能因為對端崩潰而寫到一半。
    bool closed = false;

    while (i < line.size()) {
        skipSpace(line, i);
        std::string key;
        if (!parseString(line, i, key)) {
            return false;
        }
        skipSpace(line, i);
        if (i >= line.size() || line[i] != ':') {
            return false;
        }
        ++i;

        std::string sv;
        uint64_t nv = 0;
        bool bv = false;
        int kind = -1;
        if (!parseValue(line, i, sv, nv, bv, kind)) {
            return false;
        }

        if (key == "type" && kind == 0) {
            out.type = sv;
        } else if (key == "session" && kind == 1) {
            out.session = nv;
            out.hasSession = true;
        } else if (key == "text" && kind == 0) {
            out.text = sv;
        } else if (key == "program" && kind == 0) {
            out.program = sv;
        } else if (key == "code" && kind == 0) {
            out.code = sv;
        } else if (key == "value" && kind == 0) {
            out.value = sv;
        } else if (key == "is_password" && kind == 2) {
            out.isPassword = bv;
            out.hasIsPassword = true;
        }
        // 未知欄位一律忽略——協定要能往前相容。

        skipSpace(line, i);
        if (i < line.size() && line[i] == ',') {
            ++i;
            continue;
        }
        if (i < line.size() && line[i] == '}') {
            ++i;
            closed = true;
            break;
        }
        return false;
    }

    return closed && !out.type.empty();
}

// ---------------------------------------------------------------------------
// socket path
// ---------------------------------------------------------------------------

std::string socketPath() {
    // SDD §4.3 / §7: $XDG_RUNTIME_DIR 下, 0600。
    // (SDD §3.1 的架構圖寫 ~/.local/share/voicetype/ipc.sock, 與 §4.3 不一致;
    //  以 §4.3 為準——runtime dir 才有正確的生命週期與權限語意。)
    if (const char *env = std::getenv("VOICETYPE_SOCKET")) {
        return env; // 開發時可覆寫
    }
    if (const char *rt = std::getenv("XDG_RUNTIME_DIR")) {
        return std::string(rt) + "/voicetype/ipc.sock";
    }
    // 沒有 XDG_RUNTIME_DIR 的環境 (少見) 退回 /tmp, 以 uid 隔離。
    return "/tmp/voicetype-" + std::to_string(getuid()) + "/ipc.sock";
}

// ---------------------------------------------------------------------------
// IpcClient
// ---------------------------------------------------------------------------

IpcClient::IpcClient(fcitx::EventLoop *loop, std::string socketPath,
                     MessageCallback onMessage, DisconnectCallback onDisconnect)
    : loop_(loop), path_(std::move(socketPath)),
      onMessage_(std::move(onMessage)),
      onDisconnect_(std::move(onDisconnect)) {
    tryConnect();
}

IpcClient::~IpcClient() {
    ioEvent_.reset();
    reconnectTimer_.reset();
    if (fd_ >= 0) {
        close(fd_);
        fd_ = -1;
    }
}

void IpcClient::tryConnect() {
    // 只在此處建立/重建 io event source。
    // 重要: dropConnection() 刻意不銷毀 ioEvent_, 因為它可能正在自己的
    // callback 裡執行。銷毀集中在這裡——tryConnect 只會從建構子或
    // timer callback 進入, 兩者都不在 io callback 的堆疊上。
    ioEvent_.reset();
    if (fd_ >= 0) {
        close(fd_);
        fd_ = -1;
    }
    readBuf_.clear();
    writeBuf_.clear();
    connecting_ = false;

    if (path_.size() >= sizeof(sockaddr_un{}.sun_path)) {
        FCITX_ERROR() << "voicetype: socket path too long: " << path_;
        return; // 不重試——這不會自己好
    }

    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
    if (fd < 0) {
        scheduleReconnect();
        return;
    }

    sockaddr_un addr{};
    addr.sun_family = AF_UNIX;
    std::memcpy(addr.sun_path, path_.c_str(), path_.size());

    int r = ::connect(fd, reinterpret_cast<sockaddr *>(&addr), sizeof(addr));
    if (r == 0) {
        fd_ = fd;
        connecting_ = false;
    } else if (errno == EINPROGRESS || errno == EAGAIN) {
        fd_ = fd;
        connecting_ = true;
    } else {
        // daemon 還沒起來是常態, 不用吵。
        close(fd);
        scheduleReconnect();
        return;
    }

    ioEvent_ = loop_->addIOEvent(
        fd_, connecting_ ? fcitx::IOEventFlag::Out : fcitx::IOEventFlag::In,
        [this](fcitx::EventSourceIO *, int, fcitx::IOEventFlags flags) {
            onIo(flags);
            return true;
        });

    if (!connecting_) {
        reconnectDelayUsec_ = kReconnectMinUsec;
        FCITX_DEBUG() << "voicetype: connected to daemon";
    }
}

void IpcClient::scheduleReconnect() {
    uint64_t delay = reconnectDelayUsec_;
    reconnectDelayUsec_ = std::min(reconnectDelayUsec_ * 2, kReconnectMaxUsec);

    uint64_t when = fcitx::now(CLOCK_MONOTONIC) + delay;
    if (!reconnectTimer_) {
        reconnectTimer_ = loop_->addTimeEvent(
            CLOCK_MONOTONIC, when, 0,
            [this](fcitx::EventSourceTime *, uint64_t) {
                tryConnect();
                return true;
            });
    } else {
        auto *timer = static_cast<fcitx::EventSourceTime *>(reconnectTimer_.get());
        timer->setTime(when);
        timer->setOneShot();
    }
}

void IpcClient::onIo(fcitx::IOEventFlags flags) {
    if (connecting_) {
        onConnectResult();
        return;
    }

    if (flags & fcitx::IOEventFlag::Err || flags & fcitx::IOEventFlag::Hup) {
        dropConnection(/*notify=*/true);
        return;
    }
    if (flags & fcitx::IOEventFlag::Out) {
        flushWrites();
        if (fd_ < 0) {
            return;
        }
    }
    if (flags & fcitx::IOEventFlag::In) {
        drainReads();
    }
}

void IpcClient::onConnectResult() {
    int err = 0;
    socklen_t len = sizeof(err);
    if (getsockopt(fd_, SOL_SOCKET, SO_ERROR, &err, &len) < 0 || err != 0) {
        dropConnection(/*notify=*/false);
        return;
    }
    connecting_ = false;
    reconnectDelayUsec_ = kReconnectMinUsec;
    FCITX_DEBUG() << "voicetype: connected to daemon";
    updateIoFlags();
    flushWrites();
}

void IpcClient::drainReads() {
    char buf[4096];
    for (;;) {
        ssize_t n = ::read(fd_, buf, sizeof(buf));
        if (n > 0) {
            readBuf_.append(buf, static_cast<size_t>(n));

            for (;;) {
                auto pos = readBuf_.find('\n');
                if (pos == std::string::npos) {
                    if (readBuf_.size() > kMaxLineBytes) {
                        FCITX_ERROR() << "voicetype: oversized message, dropping "
                                         "connection";
                        dropConnection(/*notify=*/true);
                        return;
                    }
                    break;
                }
                std::string line = readBuf_.substr(0, pos);
                readBuf_.erase(0, pos + 1);
                handleLine(std::move(line));
                // handleLine 的 callback 可能導致斷線 (例如 send 失敗)
                if (fd_ < 0) {
                    return;
                }
            }
            continue;
        }
        if (n == 0) {
            dropConnection(/*notify=*/true); // daemon 關閉了連線
            return;
        }
        if (errno == EINTR) {
            continue;
        }
        if (errno == EAGAIN || errno == EWOULDBLOCK) {
            return;
        }
        dropConnection(/*notify=*/true);
        return;
    }
}

void IpcClient::flushWrites() {
    while (!writeBuf_.empty()) {
        ssize_t n = ::send(fd_, writeBuf_.data(), writeBuf_.size(), MSG_NOSIGNAL);
        if (n > 0) {
            writeBuf_.erase(0, static_cast<size_t>(n));
            continue;
        }
        if (n < 0 && errno == EINTR) {
            continue;
        }
        if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
            break; // 核心緩衝滿, 等下一次 Out 事件
        }
        dropConnection(/*notify=*/true);
        return;
    }
    updateIoFlags();
}

void IpcClient::updateIoFlags() {
    if (!ioEvent_ || fd_ < 0) {
        return;
    }
    fcitx::IOEventFlags flags = fcitx::IOEventFlag::In;
    if (!writeBuf_.empty()) {
        flags |= fcitx::IOEventFlag::Out;
    }
    ioEvent_->setEvents(flags);
}

void IpcClient::handleLine(std::string line) {
    if (line.empty() || line == "\r") {
        return;
    }
    if (!line.empty() && line.back() == '\r') {
        line.pop_back();
    }
    IpcMessage msg;
    if (!parse(line, msg)) {
        FCITX_WARN() << "voicetype: malformed message from daemon";
        return;
    }
    if (onMessage_) {
        onMessage_(msg);
    }
}

bool IpcClient::send(const IpcMessage &msg) {
    if (fd_ < 0) {
        return false;
    }
    std::string payload = serialize(msg);
    payload += '\n';

    // 連線協商中: 先緩衝, 連上後 flushWrites 會送出。
    writeBuf_ += payload;
    if (connecting_) {
        return true;
    }
    flushWrites();
    return fd_ >= 0;
}

void IpcClient::dropConnection(bool notify) {
    // 刻意不銷毀 ioEvent_ (見 tryConnect 的說明)。停用即可, 實際銷毀
    // 延到下一次 tryConnect。
    if (ioEvent_) {
        ioEvent_->setEnabled(false);
    }
    if (fd_ >= 0) {
        close(fd_);
        fd_ = -1;
    }
    connecting_ = false;
    readBuf_.clear();
    writeBuf_.clear();

    if (notify && onDisconnect_) {
        onDisconnect_();
    }
    scheduleReconnect();
}

} // namespace voicetype
