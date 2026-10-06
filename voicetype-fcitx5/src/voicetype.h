#ifndef _VOICETYPE_VOICETYPE_H_
#define _VOICETYPE_VOICETYPE_H_

#include <fcitx-utils/key.h>
#include <fcitx-utils/trackableobject.h>
#include <fcitx-config/configuration.h>
#include <fcitx/addonfactory.h>
#include <fcitx/addoninstance.h>
#include <fcitx/addonmanager.h>
#include <fcitx/event.h>
#include <fcitx/inputcontext.h>
#include <fcitx/instance.h>
#include <fcitx/text.h>

#include <cstdint>
#include <functional>
#include <memory>
#include <optional>
#include <string>

#include "ipc.h"
#include "context.h"
#include "selection.h"
#include "capslock.h"

namespace voicetype {

FCITX_CONFIGURATION_CLASS(VoiceTypeConfig,
    fcitx::Option<fcitx::Key, fcitx::KeyConstrain> learnKey{
        this, "LearnKey", "Shortcut for learning a confirmed correction",
        fcitx::Key("Control+Caps_Lock"), fcitx::KeyConstrain{fcitx::KeyConstrainFlags{}}};
);

class VoiceType : public fcitx::AddonInstance {
public:
    using NotificationSink = std::function<void(const std::string &, const std::string &)>;
    using Clock = std::function<uint64_t()>;
    explicit VoiceType(fcitx::Instance *instance, NotificationSink notification = {},
                       Clock clock = {},
                       std::optional<VoiceTypeConfig> configuration = std::nullopt);
    ~VoiceType() override;

    void reloadConfig() override;
    const fcitx::Configuration *getConfig() const override { return &config_; }
    void setConfig(const fcitx::RawConfig &raw) override;

private:
    // --- 事件處理 ---
    bool onKeyEvent(fcitx::KeyEvent &event);
    void onDaemonMessage(const IpcMessage &msg);
    void onDaemonDisconnected();

    // --- 動作 ---
    void startRecording(fcitx::InputContext *ic);
    void stopRecording(bool cancelled);
    void clearDelivery();
    void deliver(const std::string &text);
    std::string contextId(fcitx::InputContext *ic) const;
    void clearCorrection();
    void maybeLearnCorrection(fcitx::InputContext *ic, bool explicitSelection,
                              std::optional<Correction> beforeClear = std::nullopt);
    void readExplicitSelection(fcitx::InputContext *ic);
    void sendCorrection(fcitx::InputContext *ic, const Correction &correction, bool confirmed);
    void notify(fcitx::InputContext *ic, const std::string &summary,
                const std::string &body);
    void clearFeedback();
    uint64_t nowUsec() const;
    std::string learnKeyLabel() const;

    // --- 狀態 ---
    fcitx::Instance *instance_;
    std::unique_ptr<IpcClient> ipc_;
    std::unique_ptr<SelectionReader> selectionReader_;
    std::unique_ptr<CapsLockGuard> capsLockGuard_;
    NotificationSink notification_;
    Clock clock_;
    VoiceTypeConfig config_;
    uint32_t notificationId_ = 0;
    fcitx::TrackableObjectReference<fcitx::InputContext> feedbackIc_;
    fcitx::Text feedbackPrevious_;
    fcitx::Text feedbackRendered_;
    std::unique_ptr<fcitx::EventSourceTime> feedbackTimer_;

    // PTT 熱鍵是 Control+Alt (兩個 modifier, 沒有一般按鍵), 判定寫在
    // voicetype.cpp 的 isCtrlAltPress/isCtrlAltRelease —— 純 modifier
    // 組合無法用 fcitx::Key::check() 表達, 所以這裡沒有對應的 Key 成員。
    //
    // 已知的取捨: Control+Alt 是桌面環境許多快捷鍵的前綴
    // (Ctrl+Alt+T 開終端、Ctrl+Alt+方向鍵 切工作區、Ctrl+Alt+F1 切 tty)。
    // 按住它說話時再碰到其他鍵, 那些快捷鍵**仍然會觸發** —— modifier
    // 狀態由 X server 維護, 不因為我們吞掉 press 事件而改變。
    // 換來的是好按。誤觸的代價由 daemon 端的 VAD 吸收: 沒有語音就
    // 不會有東西被 commit。
    //
    fcitx::Key cancelKey_{"Escape"};
    fcitx::Key learnKey_;

    bool recording_ = false;
    // A null weak target alone cannot distinguish destruction from cancellation
    // or an already consumed result. Only a pending session may deliver/fallback.
    bool deliveryPending_ = false;
    uint64_t sessionId_ = 0;

    // 錄音期間必須「鎖住」目標 InputContext。使用者放開熱鍵後到文字送達
    // 之間可能已經切換視窗, 此時不應寫進新視窗。弱參考在 IC 被銷毀時
    // 自動失效, 同時避免 use-after-free。
    fcitx::TrackableObjectReference<fcitx::InputContext> targetIc_;
    fcitx::TrackableObjectReference<fcitx::InputContext> lastCommitIc_;
    CorrectionTracker correctionTracker_;
    std::string lastCommitText_;
    uint64_t lastCommitSession_ = 0;
    uint64_t lastCommitAt_ = 0;
    std::string contextNonce_;
    // The generation belongs to this field, including synchronous frontend
    // callbacks after a result is consumed. Other ICs cannot invalidate it.
    fcitx::TrackableObjectReference<fcitx::InputContext> contextIc_;
    uint64_t contextGeneration_ = 0;
    std::string sessionContextId_;

    std::unique_ptr<fcitx::HandlerTableEntry<fcitx::EventHandler>> keyHandler_;
    std::unique_ptr<fcitx::HandlerTableEntry<fcitx::EventHandler>>
        focusOutHandler_;
    std::unique_ptr<fcitx::HandlerTableEntry<fcitx::EventHandler>>
        surroundingHandler_;
    std::unique_ptr<fcitx::HandlerTableEntry<fcitx::EventHandler>>
        resetHandler_;
    std::unique_ptr<fcitx::HandlerTableEntry<fcitx::EventHandler>>
        capabilityHandler_;
};

class VoiceTypeFactory : public fcitx::AddonFactory {
public:
    fcitx::AddonInstance *create(fcitx::AddonManager *manager) override {
        return new VoiceType(manager->instance());
    }
};

} // namespace voicetype

#endif
