#include "voicetype.h"

#include <fcitx-utils/log.h>
#include <fcitx/inputpanel.h>

namespace voicetype {

namespace {

// PTT 熱鍵是 **Control+Alt**, 兩個 modifier 而沒有一般按鍵。
//
// 這不能用 `fcitx::Key::check()` 比對: 那是為「modifier + 一般鍵」設計
// 的, 而純 modifier 組合沒有「主鍵」—— 實際送出的事件是後按下的那個
// modifier 自己 (keysym = Alt_L), 前一個 modifier 只出現在 state 裡。
// 而使用者按的順序不固定, 所以兩種順序都要認。
bool isCtrlAltPress(const fcitx::Key &key) {
    const auto sym = key.sym();
    const auto states = key.states();
    if (sym == FcitxKey_Alt_L || sym == FcitxKey_Alt_R) {
        return states.test(fcitx::KeyState::Ctrl);
    }
    if (sym == FcitxKey_Control_L || sym == FcitxKey_Control_R) {
        return states.test(fcitx::KeyState::Alt);
    }
    return false;
}

// 放開任一個 modifier 就結束錄音。
//
// 只比對 keysym 不比對 state: 放開時 state 反映的是放開**之前**的狀態,
// 而使用者不會精準地同時鬆開兩個鍵。這安全的前提是 recording_ 為真
// 代表先前確實有一次完整的 press 比對通過。
bool isCtrlAltRelease(const fcitx::Key &key) {
    const auto sym = key.normalize().sym();
    return sym == FcitxKey_Alt_L || sym == FcitxKey_Alt_R ||
           sym == FcitxKey_Control_L || sym == FcitxKey_Control_R;
}

} // namespace

VoiceType::VoiceType(fcitx::Instance *instance) : instance_(instance) {

    // --- 1. 攔截按鍵 ---
    // PreInputMethod: 在輸入法引擎 (注音等) 處理之前先看到按鍵,
    // 否則 PTT 熱鍵會被輸入法吃掉。
    keyHandler_ = instance_->watchEvent(
        fcitx::EventType::InputContextKeyEvent,
        fcitx::EventWatcherPhase::PreInputMethod, [this](fcitx::Event &event) {
            auto &keyEvent = static_cast<fcitx::KeyEvent &>(event);
            if (onKeyEvent(keyEvent)) {
                // 阻止此按鍵繼續傳遞給輸入法與應用程式
                keyEvent.filterAndAccept();
            }
        });

    // --- 2. 焦點離開時取消錄音 ---
    focusOutHandler_ = instance_->watchEvent(
        fcitx::EventType::InputContextFocusOut,
        fcitx::EventWatcherPhase::Default, [this](fcitx::Event &event) {
            auto &icEvent = static_cast<fcitx::InputContextEvent &>(event);
            if (recording_ && targetIc_.get() == icEvent.inputContext()) {
                stopRecording(/*cancelled=*/true);
            }
        });

    // --- 3. 連線 daemon, 並把 socket fd 掛進 fcitx5 的事件迴圈 ---
    // 不開執行緒。回呼在主執行緒執行, 可以安全呼叫 commitString()。
    ipc_ = std::make_unique<IpcClient>(
        &instance_->eventLoop(), socketPath(),
        [this](const IpcMessage &m) { onDaemonMessage(m); },
        [this]() { onDaemonDisconnected(); });
}

VoiceType::~VoiceType() = default;

void VoiceType::reloadConfig() {
    // M0 的熱鍵是硬編碼的, 設定全在 daemon 端的 TOML。
    // addon 自身的設定 (熱鍵) 屬於 M1。
}

bool VoiceType::onKeyEvent(fcitx::KeyEvent &event) {
    auto key = event.key();

    // 錄音中的取消鍵
    if (recording_ && !event.isRelease() && key.check(cancelKey_)) {
        stopRecording(/*cancelled=*/true);
        return true;
    }

    if (!event.isRelease()) {
        if (!isCtrlAltPress(key)) {
            return false;
        }
        // 按下: 忽略 auto-repeat
        if (recording_) {
            return true;
        }

        // SDD §4.3: daemon 不可用時不攔截熱鍵, 直接放行給輸入法,
        // 使用者不會感覺到按鍵失效。
        if (!ipc_ || !ipc_->connected()) {
            FCITX_DEBUG() << "voicetype: daemon unavailable, passing key through";
            return false;
        }

        auto *ic = event.inputContext();

        // SDD §7: 密碼欄位直接拒絕啟動錄音。
        // (SDD §4.2.4 的範例把 is_password 交給 daemon 判斷; 在 addon 端
        //  就擋掉更好——麥克風根本不會啟動, 音訊不會離開這個決策點。
        //  flag 仍會在 start 訊息中傳給 daemon 作為縱深防禦。)
        if (ic->capabilityFlags().test(fcitx::CapabilityFlag::Password)) {
            FCITX_DEBUG() << "voicetype: refusing to record in password field";
            return true; // 攔下按鍵, 但不錄音
        }

        startRecording(ic);
        return true;
    }

    // 放開
    if (!recording_) {
        return false;
    }
    if (!isCtrlAltRelease(key)) {
        return false;
    }
    stopRecording(/*cancelled=*/false);
    return true;
}

void VoiceType::startRecording(fcitx::InputContext *ic) {
    recording_ = true;
    sessionId_++;
    targetIc_ = ic->watch();

    IpcMessage msg;
    msg.type = "start";
    msg.setSession(sessionId_);
    // program() 是 fcitx5 用來做 per-program 輸入法切換的既有機制,
    // 這裡拿來做 per-app profile (SDD §4.5)。部分應用程式可能回空字串,
    // daemon 端會退回 default profile。
    msg.program = ic->program();
    msg.isPassword = false; // 密碼欄位在上面已經擋掉
    msg.hasIsPassword = true;

    if (!ipc_->send(msg)) {
        // 送不出去就別讓自己停在 recording 狀態
        recording_ = false;
        targetIc_.unwatch();
    }
}

void VoiceType::stopRecording(bool cancelled) {
    recording_ = false;

    IpcMessage msg;
    msg.type = cancelled ? "cancel" : "stop";
    msg.setSession(sessionId_);
    ipc_->send(msg);

    if (cancelled) {
        targetIc_.unwatch();
    }
}

void VoiceType::onDaemonMessage(const IpcMessage &msg) {
    if (msg.type == "result") {
        // 丟棄過期結果: 使用者已經開始下一次錄音, 舊結果不該蓋掉新的。
        if (!msg.hasSession || msg.session != sessionId_) {
            FCITX_DEBUG() << "voicetype: dropping stale result";
            return;
        }
        deliver(msg.text);
    } else if (msg.type == "error") {
        // 使用者可見的提示由 daemon 發 D-Bus notification——addon 保持極薄,
        // 不引入 notification addon 依賴 (SDD §3.2)。
        FCITX_WARN() << "voicetyped error [" << msg.code << "]: " << msg.text;
        if (msg.hasSession && msg.session == sessionId_) {
            targetIc_.unwatch();
        }
    } else if (msg.type == "state") {
        FCITX_DEBUG() << "voicetyped state: " << msg.value;
    }
}

void VoiceType::onDaemonDisconnected() {
    FCITX_INFO() << "voicetype: daemon disconnected";
    if (recording_) {
        recording_ = false;
        targetIc_.unwatch();
    }
}

void VoiceType::deliver(const std::string &text) {
    if (text.empty()) {
        targetIc_.unwatch();
        return;
    }

    auto *ic = targetIc_.get();
    if (!ic) {
        // 目標視窗已消失 → 降級鏈② (SDD §4.8): 交給 daemon 放進剪貼簿並通知。
        IpcMessage msg;
        msg.type = "fallback_clipboard";
        msg.text = text;
        ipc_->send(msg);
        return;
    }

    // commitString 是唯一的正規路徑 (SDD §2/C1, §4.8)。
    // 注意: 它沒有回傳值, 因此「應用程式收下了但沒顯示」這種失敗
    // 無法在此偵測——那是設計的固有限制, 只能靠相容性矩陣 (§8.3) 涵蓋。
    //
    // 若當前有 preedit (注音打到一半), 先清掉避免文字交錯。
    if (!ic->inputPanel().empty()) {
        ic->inputPanel().reset();
        ic->updatePreedit();
    }
    ic->commitString(text);
    targetIc_.unwatch();
}

} // namespace voicetype

// SDD §4.2.4 寫的是 FCITX_ADDON_FACTORY_V2(name, factory) —— 那個雙參數
// 版本在本機的 fcitx5 5.1.7 並不存在, addoninstance.h 只提供單參數的
// FCITX_ADDON_FACTORY。這正是 SDD §10/R3 所指的「Fcitx5 沒有穩定 ABI
// 保證」: 跨版本升級可能需要重編甚至改原始碼。
FCITX_ADDON_FACTORY(voicetype::VoiceTypeFactory)
