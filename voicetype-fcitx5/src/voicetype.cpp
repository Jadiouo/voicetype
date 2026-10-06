#include "voicetype.h"

#include <fcitx-config/iniparser.h>
#include <fcitx-utils/log.h>
#include <fcitx-utils/utf8.h>
#include <fcitx/inputpanel.h>
#include <fcitx/surroundingtext.h>
#include <fcitx/userinterface.h>

#ifdef VOICETYPE_HAVE_NOTIFICATIONS
#include <notifications_public.h>
#endif

#include <unistd.h>
#include <algorithm>
#include <vector>

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

bool isSensitive(fcitx::InputContext *ic) {
    return ic->capabilityFlags().test(fcitx::CapabilityFlag::Password) ||
           ic->capabilityFlags().test(fcitx::CapabilityFlag::Sensitive);
}

std::optional<TextSnapshot> snapshot(fcitx::InputContext *ic) {
    if (!ic || isSensitive(ic) ||
        !ic->capabilityFlags().test(fcitx::CapabilityFlag::SurroundingText)) {
        return std::nullopt;
    }
    const auto &s = ic->surroundingText();
    if (!s.isValid() || s.text().size() > 256 * 1024) {
        return std::nullopt;
    }
    return TextSnapshot{s.text(), s.cursor(), s.anchor()};
}

std::optional<EditIntent> editingIntent(const fcitx::KeyEvent &event) {
    // rawKey is after layout conversion but before normalization, preserving
    // uppercase/shifted ASCII. Unknown forwarding/composition is not a literal.
    const auto raw = event.rawKey();
    const auto allowed = fcitx::KeyStates(fcitx::KeyState::Shift) |
        fcitx::KeyState::CapsLock | fcitx::KeyState::NumLock | fcitx::KeyState::Repeat;
    if (event.forward() || event.isVirtual() || (raw.states() & ~allowed)) { return std::nullopt; }
    if (raw.sym() == FcitxKey_BackSpace) { return EditIntent{EditIntent::Kind::Backspace}; }
    if (raw.sym() == FcitxKey_Delete && !raw.states().test(fcitx::KeyState::Shift)) {
        return EditIntent{EditIntent::Kind::Delete};
    }
    const auto unicode = fcitx::Key::keySymToUnicode(raw.sym());
    if (unicode >= 0x20 && unicode <= 0x7e) {
        return EditIntent{EditIntent::Kind::Insert, static_cast<char>(unicode)};
    }
    return std::nullopt;
}

bool sameText(const fcitx::Text &left, const fcitx::Text &right) {
    if (left.size() != right.size() || left.cursor() != right.cursor()) { return false; }
    for (size_t i = 0; i < left.size(); ++i) {
        if (left.stringAt(i) != right.stringAt(i) || left.formatAt(i) != right.formatAt(i)) {
            return false;
        }
    }
    return true;
}

std::string safeErrorBody(const std::string &source) {
    if (source.empty() || !fcitx::utf8::validate(source)) {
        return "辨識或文字處理失敗，請重試。";
    }
    std::string result;
    size_t characters = 0;
    for (unsigned char c : source) {
        if ((c & 0xc0) != 0x80 && ++characters > 240) {
            result += "…";
            break;
        }
        if (c < 0x20 || c == 0x7f) { result += ' '; }
        else if (c == '<') { result += "&lt;"; }
        else if (c == '>') { result += "&gt;"; }
        else if (c == '&') { result += "&amp;"; }
        else { result += static_cast<char>(c); }
    }
    return result;
}

} // namespace

VoiceType::VoiceType(fcitx::Instance *instance, NotificationSink notification,
                     Clock clock, std::optional<VoiceTypeConfig> configuration)
    : instance_(instance), notification_(std::move(notification)), clock_(std::move(clock)) {
    if (configuration) {
        config_ = *configuration;
    } else {
        fcitx::readAsIni(config_, "conf/voicetype.conf");
    }
    learnKey_ = *config_.learnKey;
    capsLockGuard_ = std::make_unique<CapsLockGuard>(instance_);
    selectionReader_ = std::make_unique<SelectionReader>(&instance_->eventLoop());
    contextNonce_ = std::to_string(getpid()) + "-" +
                    std::to_string(fcitx::now(CLOCK_MONOTONIC));

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
            if (contextIc_.get() == icEvent.inputContext()) { ++contextGeneration_; }
            if (deliveryPending_ && targetIc_.get() == icEvent.inputContext()) {
                stopRecording(/*cancelled=*/true);
            }
            if (lastCommitIc_.get() == icEvent.inputContext()) {
                clearCorrection();
            }
        });

    surroundingHandler_ = instance_->watchEvent(
        fcitx::EventType::InputContextSurroundingTextUpdated,
        fcitx::EventWatcherPhase::Default, [this](fcitx::Event &event) {
            auto *ic = static_cast<fcitx::InputContextEvent &>(event).inputContext();
            if (ic != lastCommitIc_.get()) {
                return;
            }
            if (const auto s = snapshot(ic)) {
                if (auto candidate = correctionTracker_.correctionBeforeClear(*s, nowUsec())) {
                    maybeLearnCorrection(ic, false, std::move(candidate));
                }
                correctionTracker_.observe(*s, nowUsec());
            } else {
                // Some frontends publish no valid surrounding snapshot at all.
                // Drop automatic attribution, retaining only the bounded last
                // delivery for a later explicit selected-sentence confirmation.
                correctionTracker_.clear();
            }
        });
    resetHandler_ = instance_->watchEvent(
        fcitx::EventType::InputContextReset,
        fcitx::EventWatcherPhase::Default, [this](fcitx::Event &event) {
            auto *ic = static_cast<fcitx::InputContextEvent &>(event).inputContext();
            if (contextIc_.get() == ic) { ++contextGeneration_; }
            if (deliveryPending_ && targetIc_.get() == ic) {
                stopRecording(/*cancelled=*/true);
            }
            if (ic == lastCommitIc_.get()) {
                clearCorrection();
            }
        });
    capabilityHandler_ = instance_->watchEvent(
        fcitx::EventType::InputContextCapabilityChanged,
        fcitx::EventWatcherPhase::Default, [this](fcitx::Event &event) {
            auto *ic = static_cast<fcitx::InputContextEvent &>(event).inputContext();
            if (contextIc_.get() == ic && isSensitive(ic)) { ++contextGeneration_; }
            if (deliveryPending_ && ic == targetIc_.get() && isSensitive(ic)) {
                stopRecording(/*cancelled=*/true);
            }
            if (ic == lastCommitIc_.get() && isSensitive(ic)) {
                clearCorrection();
            }
        });

    // --- 3. 連線 daemon, 並把 socket fd 掛進 fcitx5 的事件迴圈 ---
    // 不開執行緒。回呼在主執行緒執行, 可以安全呼叫 commitString()。
    ipc_ = std::make_unique<IpcClient>(
        &instance_->eventLoop(), socketPath(),
        [this](const IpcMessage &m) { onDaemonMessage(m); },
        [this]() { onDaemonDisconnected(); });
}

VoiceType::~VoiceType() { capsLockGuard_.reset(); selectionReader_.reset(); clearFeedback(); }

void VoiceType::reloadConfig() {
    fcitx::readAsIni(config_, "conf/voicetype.conf");
    learnKey_ = *config_.learnKey;
}

void VoiceType::setConfig(const fcitx::RawConfig &raw) {
    config_.load(raw, true);
    fcitx::safeSaveAsIni(config_, "conf/voicetype.conf");
    learnKey_ = *config_.learnKey;
}

std::string VoiceType::learnKeyLabel() const {
    return learnKey_.toString(fcitx::KeyStringFormat::Localized);
}

bool VoiceType::onKeyEvent(fcitx::KeyEvent &event) {
    auto key = event.key();
    const bool learnPress = !event.isRelease() && key.check(learnKey_);
    if (capsLockGuard_->filter(event, learnPress)) { return true; }
    if (learnPress) {
        auto *ic = event.inputContext();
        clearFeedback();
        if (isSensitive(ic)) {
            notify(ic, "此欄位無法學習", "請在一般文字欄位使用詞彙學習。");
        } else if (recording_) {
            notify(ic, "正在錄音，尚未學習", "請先放開語音鍵，等文字送出並修正後再按 " + learnKeyLabel() + "。");
        } else if (!ipc_ || !ipc_->connected()) {
            notify(ic, "語音服務未連線", "這次修正尚未送出，請確認 VoiceType 服務已啟動後再試。");
        } else {
            maybeLearnCorrection(ic, true);
        }
        return true;
    }
    if (!event.isRelease() && event.inputContext() == lastCommitIc_.get()) {
        const auto sym = key.sym();
        const bool clipboardOrUndo =
            (key.states().test(fcitx::KeyState::Ctrl) &&
             (sym == FcitxKey_v || sym == FcitxKey_V || sym == FcitxKey_x ||
              sym == FcitxKey_X || sym == FcitxKey_z || sym == FcitxKey_Z ||
              sym == FcitxKey_y || sym == FcitxKey_Y)) ||
            (key.states().test(fcitx::KeyState::Shift) &&
             (sym == FcitxKey_Insert || sym == FcitxKey_Delete));
        const auto &panel = event.inputContext()->inputPanel();
        const bool composing = !panel.preedit().empty() || !panel.clientPreedit().empty() || panel.candidateList();
        if (clipboardOrUndo || sym == FcitxKey_Escape || composing) {
            // Keep lastCommitText_ for the explicit LearnKey workflow.
            correctionTracker_.clear();
        } else if (auto intent = editingIntent(event)) {
            if (const auto before = snapshot(event.inputContext())) {
                correctionTracker_.noteUserEditKey(*intent, *before, nowUsec());
            } else { correctionTracker_.clear(); }
        } else if (sym == FcitxKey_BackSpace || sym == FcitxKey_Delete ||
                   fcitx::Key::keySymToUnicode(event.rawKey().sym()) >= 0x20) {
            // Unsupported text/word/modified edits cannot authorize a later
            // arbitrary update; explicit selected-text confirmation remains.
            correctionTracker_.clear();
        } else if ((sym == FcitxKey_Return || sym == FcitxKey_KP_Enter) &&
                   !key.states().test(fcitx::KeyState::Shift) &&
                   !key.states().test(fcitx::KeyState::Alt)) {
            const auto &panel = event.inputContext()->inputPanel();
            if (panel.preedit().empty() && panel.clientPreedit().empty() && !panel.candidateList()) {
                correctionTracker_.noteSubmitKey(nowUsec());
            }
        }
    }

    // Escape also cancels a released recording while its result is pending.
    if (deliveryPending_ && !event.isRelease() && key.check(cancelKey_)) {
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
            notify(event.inputContext(), "語音服務未連線", "尚未開始錄音，請確認 VoiceType 服務已啟動後再試。");
            return false;
        }

        auto *ic = event.inputContext();

        // A stale frontend key cannot create a new delivery session for an
        // unfocused field, even if that field regains focus before the result.
        if (!ic->hasFocus()) { return false; }

        // SDD §7: 密碼欄位直接拒絕啟動錄音。
        // (SDD §4.2.4 的範例把 is_password 交給 daemon 判斷; 在 addon 端
        //  就擋掉更好——麥克風根本不會啟動, 音訊不會離開這個決策點。
        //  flag 仍會在 start 訊息中傳給 daemon 作為縱深防禦。)
        if (isSensitive(ic)) {
            FCITX_DEBUG() << "voicetype: refusing to record in password field";
            notify(ic, "此欄位無法使用語音輸入", "請在一般文字欄位使用語音輸入。");
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
    maybeLearnCorrection(ic, false);
    clearCorrection();
    recording_ = true;
    deliveryPending_ = true;
    sessionId_++;
    targetIc_ = ic->watch();
    contextIc_ = ic->watch();

    IpcMessage msg;
    msg.type = "start";
    msg.setSession(sessionId_);
    // program() 是 fcitx5 用來做 per-program 輸入法切換的既有機制,
    // 這裡拿來做 per-app profile (SDD §4.5)。部分應用程式可能回空字串,
    // daemon 端會退回 default profile。
    msg.program = ic->program();
    msg.isPassword = false; // 密碼欄位在上面已經擋掉
    msg.hasIsPassword = true;
    sessionContextId_ = contextId(ic);
    msg.contextId = sessionContextId_;
    if (const auto s = snapshot(ic)) {
        if (const auto context = boundedContext(*s)) {
            msg.contextText = context->text;
            msg.selectedText = context->selection;
        }
    }

    if (!ipc_->send(msg)) {
        // 送不出去就別讓自己停在 recording 狀態
        clearDelivery();
        notify(ic, "錄音要求未送出", "語音服務連線中斷，請稍後再試。");
    }
}

void VoiceType::stopRecording(bool cancelled) {
    IpcMessage msg;
    msg.type = cancelled ? "cancel" : "stop";
    msg.setSession(sessionId_);
    if (cancelled) {
        clearDelivery();
    } else {
        recording_ = false;
    }
    // send() can synchronously report disconnect. Invalidate first.
    ipc_->send(msg);
}

void VoiceType::clearDelivery() {
    recording_ = false;
    deliveryPending_ = false;
    targetIc_.unwatch();
    sessionContextId_.clear();
}

void VoiceType::onDaemonMessage(const IpcMessage &msg) {
    if (msg.type == "desktop_hello" && msg.hasSession) {
        IpcMessage reply;
        reply.type = "desktop_hello";
        reply.setSession(msg.session);
        reply.value = "voicetype.fcitx.v1";
        ipc_->send(reply);
        return;
    }
    if (msg.type == "deliver" && msg.hasSession) {
        IpcMessage reply;
        reply.type = "delivered";
        reply.setSession(msg.session);
        reply.contextId = msg.contextId;
        if (!deliveryPending_ || msg.session != sessionId_) {
            reply.code = "stale";
        } else if (msg.contextId.empty() || msg.contextId != sessionContextId_) {
            clearDelivery();
            reply.code = "focus_changed";
        } else {
            reply.code = deliver(msg.text, false);
        }
        // The acknowledgement describes the original request even when a
        // synchronous frontend callback starts another session during commit.
        ipc_->send(reply);
        return;
    }
    if (msg.type == "result") {
        // 丟棄過期結果: 使用者已經開始下一次錄音, 舊結果不該蓋掉新的。
        if (!deliveryPending_ || !msg.hasSession || msg.session != sessionId_) {
            FCITX_DEBUG() << "voicetype: dropping stale result";
            return;
        }
        deliver(msg.text);
    } else if (msg.type == "error") {
        // Learning/control errors already have daemon notifications and no live
        // delivery target. A stale ASR error must not cancel a newer dictation.
        if (!deliveryPending_ || !msg.hasSession || msg.session != sessionId_) {
            return;
        }
        auto *ic = targetIc_.get();
        const auto expectedContext = sessionContextId_;
        clearDelivery();
        if (ic && msg.code != "empty_result" && ic->hasFocus() &&
            contextId(ic) == expectedContext && !isSensitive(ic)) {
            notify(ic, "語音輸入未送出", safeErrorBody(msg.text));
        }
    } else if (msg.type == "state") {
        FCITX_DEBUG() << "voicetyped state: " << msg.value;
    }
}

void VoiceType::onDaemonDisconnected() {
    FCITX_INFO() << "voicetype: daemon disconnected";
    ++contextGeneration_;
    clearDelivery();
    clearCorrection();
}

const char *VoiceType::deliver(const std::string &text, bool allowLegacyFallback) {
    const auto expectedContext = sessionContextId_;
    const auto expectedSession = sessionId_;
    auto target = targetIc_;
    auto *ic = target.get();
    // Consume before commit/IPC callbacks: one result has at most one effect.
    clearDelivery();
    if (text.empty() || (!allowLegacyFallback &&
        (text.size() > 64 * 1024 || text.find('\0') != std::string::npos || !fcitx::utf8::validate(text)))) {
        return "invalid_text";
    }
    if (!ic) {
        // 目標視窗已消失 → 降級鏈② (SDD §4.8): 交給 daemon 放進剪貼簿並通知。
        if (allowLegacyFallback) {
            IpcMessage msg;
            msg.type = "fallback_clipboard";
            msg.text = text;
            ipc_->send(msg);
        }
        return "focus_changed";
    }

    // The field may have become sensitive while CPU decoding was in progress.
    if (!ic->hasFocus() || contextId(ic) != expectedContext || isSensitive(ic)) {
        clearCorrection();
        return "focus_changed";
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
    // Frontends may synchronously reset/unfocus/destroy the field or start a
    // new session while receiving the preedit update. Recheck the weak target.
    ic = target.get();
    if (!ic || !ic->hasFocus() || contextId(ic) != expectedContext ||
        isSensitive(ic) || sessionId_ != expectedSession) {
        return "focus_changed";
    }
    clearCorrection();
    if (fcitx::utf8::validate(text) && fcitx::utf8::length(text) <= 512) {
        lastCommitIc_ = ic->watch();
        lastCommitText_ = text;
        lastCommitSession_ = sessionId_;
        lastCommitAt_ = nowUsec();
        if (const auto s = snapshot(ic)) {
            correctionTracker_.begin(*s, text, lastCommitAt_);
        }
    }
    ic->commitString(text);
    return "committed";
}

std::string VoiceType::contextId(fcitx::InputContext *ic) const {
    static constexpr char digits[] = "0123456789abcdef";
    std::string id = contextNonce_ + "-" +
                     std::to_string(contextGeneration_) + "-";
    for (auto byte : ic->uuid()) {
        id += digits[byte >> 4];
        id += digits[byte & 15];
    }
    return id;
}

void VoiceType::clearCorrection() {
    if (selectionReader_) { selectionReader_->cancel(); }
    correctionTracker_.clear();
    lastCommitIc_.unwatch();
    lastCommitText_.clear();
    lastCommitAt_ = 0;
}

void VoiceType::maybeLearnCorrection(fcitx::InputContext *ic,
                                    bool explicitSelection,
                                    std::optional<Correction> beforeClear) {
    const auto reject = [this, ic, explicitSelection](const char *summary, const std::string &body) {
        if (explicitSelection) { notify(ic, summary, body); }
    };
    if (isSensitive(ic)) {
        reject("此欄位無法學習", "請在一般文字欄位使用詞彙學習。");
        return;
    }
    if (ic != lastCommitIc_.get() || !ic->hasFocus() || lastCommitText_.empty()) {
        reject("沒有可學習的上一句", "請先語音輸入，在同一欄位修正並選取完整句子。切換視窗或欄位後，上一句不會繼續追蹤。");
        return;
    }
    const auto &panel = ic->inputPanel();
    if (!panel.preedit().empty() || !panel.clientPreedit().empty() || panel.candidateList()) {
        reject("請先完成目前的輸入", "請先確認或取消輸入法正在組字的內容，再選取修正後的完整句子。");
        return;
    }
    const auto now = nowUsec();
    if (now < lastCommitAt_ || now - lastCommitAt_ > 5 * 60 * 1000000ULL) {
        clearCorrection();
        reject("上一句已超過學習時限", "請在語音輸入後五分鐘內修正並學習；這次沒有新增規則。");
        return;
    }
    const auto s = snapshot(ic);
    if (!s) {
        if (explicitSelection) { readExplicitSelection(ic); }
        return;
    }
    std::optional<Correction> correction;
    if (explicitSelection) {
        if (s->cursor == s->anchor) {
            reject("請先選取修正後的完整句子", learnKeyLabel() + " 會學習上一句與所選句子的差異；目前沒有選取文字。");
            return;
        }
        if (std::max(s->cursor, s->anchor) - std::min(s->cursor, s->anchor) > 512) {
            reject("選取的文字太長", "請只選取上一句修正後的完整句子，最多 512 個字。");
            return;
        }
        const auto context = boundedContext(*s);
        if (!context || context->selection.empty()) {
            reject("無法讀取選取文字", "這個程式尚未提供有效的選取內容，請重新選取後再試。");
            return;
        }
        if (context->selection == lastCommitText_) {
            reject("選取的句子還沒有修正", "選取內容與上一句完全相同，這次沒有新增規則。");
            return;
        }
        correction = Correction{lastCommitText_, context->selection};
    } else {
        correction = beforeClear ? std::move(beforeClear)
                                 : correctionTracker_.correction(*s, now);
    }
    if (!correction) {
        return;
    }
    sendCorrection(ic, *correction, explicitSelection);
}

void VoiceType::sendCorrection(fcitx::InputContext *ic, const Correction &correction, bool explicitSelection) {
    IpcMessage msg;
    msg.type = "correction";
    msg.setSession(lastCommitSession_);
    msg.program = ic->program();
    msg.contextId = contextId(ic);
    msg.before = correction.before;
    msg.after = correction.after;
    msg.hasConfirmed = true;
    msg.confirmed = explicitSelection;
    clearCorrection();
    const bool sent = ipc_->send(msg);
    if (explicitSelection) {
        if (sent) {
            notify(ic, "已送出修正，等待確認", "這還不是學習成功；VoiceType 會再通知是否已記住詞彙。");
        } else {
            notify(ic, "修正未送出", "語音服務連線中斷，這次沒有新增規則。請稍後再試。");
        }
    }
}

void VoiceType::readExplicitSelection(fcitx::InputContext *ic) {
    if (selectionReader_->busy()) {
        notify(ic, "正在讀取所選句子", "請稍候，這次尚未新增規則。");
        return;
    }
    const auto program = ic->program();
    const auto expectedContext = contextId(ic);
    const auto expectedSession = lastCommitSession_;
    const auto expectedAt = lastCommitAt_;
    const char *overridePath = std::getenv("VOICETYPE_SELECTION_HELPER");
    const char *home = std::getenv("HOME");
    const std::string helper = overridePath ? overridePath :
        (home ? std::string(home) + "/.local/bin/voicetype-selection" : "");
    const bool started = selectionReader_->start(helper, program,
        [this, program, expectedContext, expectedSession, expectedAt](const IpcMessage &reply) {
            auto *target = lastCommitIc_.get();
            const auto now = nowUsec();
            if (!target || !target->hasFocus() || isSensitive(target) || recording_ ||
                lastCommitSession_ != expectedSession || lastCommitAt_ != expectedAt ||
                contextId(target) != expectedContext || target->program() != program ||
                now < expectedAt || now - expectedAt > 5 * 60 * 1000000ULL) { return; }
            if (!target->inputPanel().preedit().empty() ||
                !target->inputPanel().clientPreedit().empty() || target->inputPanel().candidateList()) {
                notify(target, "修正未送出", "請先完成輸入法組字，再選取完整句子確認。");
                return;
            }
            if (reply.type != "selection") {
                std::string body = "目前程式未提供可核對的選取欄位；這次沒有新增規則。可使用 VoiceType 的 learn 指令。";
                if (reply.code == "no_selection") {
                    body = "請先選取上一句修正後的完整句子，再按 " + learnKeyLabel() + "。";
                } else if (reply.code == "timeout" || reply.code == "tree_limit") {
                    body = "頁面太大或讀取超時；這次沒有新增規則。請重新選取後再試，或使用 VoiceType 的 learn 指令。";
                } else if (reply.code == "selection_too_long") {
                    body = "請只選取上一句修正後的完整句子，最多 512 個字。";
                } else if (reply.code == "missing_atspi_dependency" || reply.code == "missing_xprop_dependency") {
                    body = "選取工具缺少依賴：python3-gi、gir1.2-atspi-2.0、x11-utils。這次沒有新增規則。";
                } else if (reply.code == "focus_changed" || reply.code == "selection_changed") {
                    body = "讀取時焦點或選取內容改變；請重新選取完整句子後再試。";
                }
                notify(target, "無法讀取修正，尚未學習", body);
                return;
            }
            if (reply.program != program || reply.contextId.empty() || reply.value.empty() ||
                !fcitx::utf8::validate(reply.text) || reply.text.empty() ||
                reply.text.find('\0') != std::string::npos || fcitx::utf8::length(reply.text) > 512) {
                notify(target, "修正未送出", "選取工具回傳的欄位資訊無效，這次沒有新增規則。");
                return;
            }
            if (reply.text == lastCommitText_) {
                notify(target, "選取的句子還沒有修正", "選取內容與上一句完全相同，這次沒有新增規則。");
                return;
            }
            // No original AT-SPI identity was captured at dictation delivery.
            // This is explicit user confirmation, never automatic attribution.
            sendCorrection(target, Correction{lastCommitText_, reply.text}, true);
        });
    if (!started) {
        notify(ic, "選取工具尚未安裝", "請重新執行 VoiceType addon 安裝腳本；這次沒有新增規則。");
    }
}

uint64_t VoiceType::nowUsec() const {
    return clock_ ? clock_() : fcitx::now(CLOCK_MONOTONIC);
}

void VoiceType::clearFeedback() {
    if (feedbackTimer_) { feedbackTimer_->setEnabled(false); }
    if (auto *ic = feedbackIc_.get()) {
        if (sameText(ic->inputPanel().auxDown(), feedbackRendered_)) {
            ic->inputPanel().setAuxDown(feedbackPrevious_);
            ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
        }
    }
    feedbackIc_.unwatch();
    feedbackRendered_.clear();
}

void VoiceType::notify(fcitx::InputContext *ic, const std::string &summary,
                       const std::string &body) {
    if (notification_) {
        notification_(summary, body);
        return;
    }
#ifdef VOICETYPE_HAVE_NOTIFICATIONS
    if (auto *notifications = instance_->addonManager().addon("notifications", true)) {
        notificationId_ = notifications->call<fcitx::INotifications::sendNotification>(
            "VoiceType", notificationId_, "input-microphone", summary, body,
            std::vector<std::string>{}, 4500,
            fcitx::NotificationActionCallback{}, fcitx::NotificationClosedCallback{});
        return;
    }
#endif
    // No notifications addon: temporarily append auxiliary UI text, preserving
    // existing preedit/candidates and restoring only our own untouched message.
    clearFeedback();
    if (!ic || !ic->hasFocus()) {
        FCITX_WARN() << "voicetype: " << summary << ": " << body;
        return;
    }
    feedbackIc_ = ic->watch();
    feedbackPrevious_ = ic->inputPanel().auxDown();
    fcitx::Text text = feedbackPrevious_;
    if (!text.empty()) { text.append("\n"); }
    text.append(summary + "：" + body);
    feedbackRendered_ = text;
    ic->inputPanel().setAuxDown(text);
    ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
    const auto until = fcitx::now(CLOCK_MONOTONIC) + 4500000;
    if (!feedbackTimer_) {
        feedbackTimer_ = instance_->eventLoop().addTimeEvent(CLOCK_MONOTONIC, until, 0,
            [this](fcitx::EventSourceTime *, uint64_t) { clearFeedback(); return false; });
    } else {
        feedbackTimer_->setTime(until);
        feedbackTimer_->setOneShot();
    }
}

} // namespace voicetype

// SDD §4.2.4 寫的是 FCITX_ADDON_FACTORY_V2(name, factory) —— 那個雙參數
// 版本在本機的 fcitx5 5.1.7 並不存在, addoninstance.h 只提供單參數的
// FCITX_ADDON_FACTORY。這正是 SDD §10/R3 所指的「Fcitx5 沒有穩定 ABI
// 保證」: 跨版本升級可能需要重編甚至改原始碼。
FCITX_ADDON_FACTORY(voicetype::VoiceTypeFactory)
