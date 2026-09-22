#ifndef _VOICETYPE_IPC_H_
#define _VOICETYPE_IPC_H_

#include <fcitx-utils/event.h>

#include <cstdint>
#include <functional>
#include <memory>
#include <string>

namespace voicetype {

// SDD §4.3 的線上訊息。單一結構同時服務兩個方向：未使用的欄位留空,
// 序列化時會被略過。刻意保持扁平——協定只有九種訊息, 不值得做型別階層。
struct IpcMessage {
    std::string type;

    // hasSession 區分「session 0」與「沒有 session 欄位」。
    // fallback_clipboard / ping / pong 不帶 session。
    bool hasSession = false;
    uint64_t session = 0;

    std::string text;     // result / error / fallback_clipboard
    std::string program;  // start
    std::string code;     // error
    std::string value;    // state
    std::string contextId;    // start / correction (opaque input context ID)
    std::string contextText;  // start (bounded surrounding text)
    std::string selectedText; // start
    std::string before;       // correction (full delivered dictation)
    std::string after;        // correction (full corrected dictation)
    bool hasConfirmed = false;
    bool confirmed = false;

    bool hasIsPassword = false;
    bool isPassword = false;

    void setSession(uint64_t id) {
        session = id;
        hasSession = true;
    }
};

// NDJSON 編解碼。獨立出來是為了能單獨測試——這是 addon 裡唯一有
// 實質分支邏輯的部分, 其餘都是 fcitx5 API 轉發。
std::string serialize(const IpcMessage &msg);
bool parse(const std::string &line, IpcMessage &out);

// 連向 voicetyped 的 socket client。
//
// 關鍵設計 (SDD §3.3 / §4.2.5): 不開執行緒。socket fd 直接掛進 fcitx5 的
// EventLoop, 所有回呼都在主執行緒執行, 因此回呼中可以安全呼叫
// commitString()。若改用執行緒, 必須透過 eventDispatcher().schedule()
// 才能回到主執行緒。
class IpcClient {
public:
    using MessageCallback = std::function<void(const IpcMessage &)>;
    using DisconnectCallback = std::function<void()>;

    IpcClient(fcitx::EventLoop *loop, std::string socketPath,
              MessageCallback onMessage, DisconnectCallback onDisconnect);
    ~IpcClient();

    IpcClient(const IpcClient &) = delete;
    IpcClient &operator=(const IpcClient &) = delete;

    // 送出一則訊息。daemon 未連線時回傳 false——呼叫端據此決定是否
    // 放行熱鍵 (SDD §4.3: daemon 不可用時 PTT 不攔截)。
    bool send(const IpcMessage &msg);

    bool connected() const { return fd_ >= 0 && !connecting_; }

private:
    void tryConnect();
    void scheduleReconnect();
    void onIo(fcitx::IOEventFlags flags);
    void onConnectResult();
    void drainReads();
    void flushWrites();
    void updateIoFlags();
    void handleLine(std::string line);
    void dropConnection(bool notify);

    fcitx::EventLoop *loop_;
    std::string path_;
    MessageCallback onMessage_;
    DisconnectCallback onDisconnect_;

    int fd_ = -1;
    bool connecting_ = false;

    std::string readBuf_;
    std::string writeBuf_;

    // 指數退避 100ms → 5s (SDD §4.3)
    static constexpr uint64_t kReconnectMinUsec = 100 * 1000;
    static constexpr uint64_t kReconnectMaxUsec = 5 * 1000 * 1000;
    uint64_t reconnectDelayUsec_ = kReconnectMinUsec;

    // 單則訊息上限。防止異常 daemon 讓 addon 無限制長大。
    static constexpr size_t kMaxLineBytes = 256 * 1024;

    std::unique_ptr<fcitx::EventSourceIO> ioEvent_;
    std::unique_ptr<fcitx::EventSource> reconnectTimer_;
};

// $XDG_RUNTIME_DIR/voicetype/ipc.sock (SDD §4.3, §7)
std::string socketPath();

} // namespace voicetype

#endif
