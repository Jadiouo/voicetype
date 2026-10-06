# 上下文、修正學習與本機文字校正

預設語音引擎為CPU SenseVoice，另有可選的[Nano CPU profile](NANO-PREVIEW.md)。個人詞彙學習與文字校正不修改ASR模型權重。[詞庫GUI](SETTINGS-GUI.md)可直接管理詞對；[抽樣校對](REVIEW-QUEUE.md)可保存少量錄音，稍後重聽並確認詞對。

## 平常怎麼用

照常按住Ctrl+Alt說話。輸出後，在原輸入框把錯詞改正；只有可追蹤的直接鍵盤編輯與回報文字、游標位置精確一致時，才會觀察修正。符合條件的送出／清空或下一句可完成觀察。**兩次獨立語音輸出都改成同一個詞後，才啟用自動學習規則。** 只新增詞句、整段重寫、IME組字、貼上、焦點切換或無法確定歸屬時不自動學習。詳細測試邊界見[addon測試](../voicetype-fcitx5/tests/README.md)。

想立即記住：在 VoiceType 填入文字後五分鐘內、聊天訊息送出之前，在同一輸入框，選取**完整的修正後那一句**，按 **Ctrl+Caps Lock**。能辨認單一詞彙修正時，會顯示通知並立即記住；之後套用到同一應用程式。沒有周邊文字支援或複雜改寫時，可直接指定詞彙：

```bash
voicetype-control learn mabe maybe
voicetype-control learn gthub GitHub
voicetype-control learn 陳博宇 陳柏宇 --program chrome
voicetype-control learned
voicetype-control forget 陳博宇
```

`--program` 要與 Fcitx 回報的應用程式名稱一致；省略會建立全域的明確規則。學習記錄存在 `~/.local/share/voicetype/personalization.json`，權限 0600，可列出、刪除。自動觀察形成的規則更保守：換輸入脈絡後，需要新的周邊文字再次出現正確詞，才套用。這不是跨網站的永久人物辨識；Fcitx 的輸入框 ID 不等於瀏覽器分頁或文件 ID。

規則保護英文詞界、反引號、路徑與程式識別字，也不會連鎖替換。同一個錯詞有互相衝突的修正時會保留原文。這些限制降低誤改，但不能保證所有同名或同音情況都能判斷正確。

### 怎麼知道有沒有學到

學習由手動改字觸發，不是只要一直說話就會訓練模型。請在訊息送出或輸入框清空之前修正；五分鐘是從 VoiceType 把文字填入輸入框時起算。

| 通知 | 意義 |
|---|---|
| 已記錄修正（1/2） | 觀察到第一次修正，尚未自動套用 |
| 已學會修正（2/2） | 兩次獨立修正成立，之後在相關上下文中套用 |
| 已儲存並啟用修正 | 快捷鍵確認成功，會列出「錯詞 → 正確詞」 |
| 這個修正已經啟用 | 原本就有這個規則 |
| 已送出修正，等待確認 | 只是送出要求，還要等後續結果通知 |
| 沒有新增修正／請先選取完整句子 | 尚未學到，通知會說明原因 |

若詞庫檔案無法讀取而退回暫存記憶體，通知會明確指出重啟後不保留。找不到來源應用程式時，不會把快捷鍵確認的修正偷偷擴成全域規則。未修改、選取過長、已過期、切換欄位或服務離線，也都有明確提示。

目前使用桌面通知，沒有另加提示音。成功／觀察結果需要 `notify-send`（Debian/Ubuntu 的 `libnotify-bin`）；一般安裝腳本會先檢查。快捷鍵的前置失敗提示優先使用 Fcitx Notifications，沒有該模組時顯示在輸入法提示區。桌面的勿擾設定仍可能隱藏橫幅，可用 `voicetype-control learned` 查看已啟用和待確認的規則。

預設明確學習快捷鍵仍為Ctrl+Caps Lock，可在Fcitx addon設定修改`LearnKey`。X11的Caps Lock還原只在可識別的原生按鍵事件和有限時間內運作；Wayland及不提供原始事件的frontend不保證還原。若周邊文字不可用，安裝器提供的`voicetype-selection` helper可在明確確認時讀取同一來源視窗的選取文字；沒有選取、來源不符或逾時會放棄。

## 上下文從哪裡來

- Fcitx 提供目前輸入框的周邊文字與選取文字。
- 螢幕上下文預設關閉。若要啟用，需同時設定`VOICETYPE_ENABLE_SCREEN_CONTEXT=1`和`VOICETYPE_CONTEXT_HELPER`；helper在按下錄音熱鍵時讀一次目前視窗標題與AT-SPI可見文字，沒有背景OCR或聊天歷史同步。
- 也可提供暫時上下文，三十分鐘後失效，僅存記憶體：

```bash
voicetype-control context --text '目前討論陳柏宇教授的機器人實驗室。'
voicetype-control context --file /path/to/relevant-notes.txt
voicetype-control context --text ''  # 清除
```

helper 最長等候 500 ms，與錄音並行；不讀剪貼簿、密碼欄位或畫面截圖。頁面內容與未確認的歷史 ASR 輸出不會直接建立永久詞彙規則。支援 AT-SPI 的 GTK 測試視窗已驗證能讀取可見標籤，並排除可編輯文字；不同瀏覽器／應用程式仍需各自驗證。Wayland 沒有這個 X11 helper 的頁面功能，Fcitx 的上下文與手動入口仍可使用。

## 可選的小模型

**文字模型目前是實驗選項，預設未啟用。** 驗收曾出現原本正確的英文詞與人物名稱被誤改。修正學習與上下文功能可以獨立使用。驗收方法見 [CONTEXT-VALIDATION.md](CONTEXT-VALIDATION.md)。

本機 llama.cpp + Qwen3-0.6B-Q8_0 從程式預先驗證的候選修改中選擇；模型不能自由改寫句子。預設 `faithful` 保留口語與句意；`clean` 另外允許移除有限的句首遲疑詞，**不是整段潤稿功能**。模型逾時、離線、格式不符或提出不合規的修改時保留原文；個人詞彙學習不依賴這個服務。

```bash
# 只預覽，不打字、不開麥克風、不記錄為學習樣本
voicetype-control process '這個 mabe 可以用，我要 push 到 gthub。' \
  --context 'GitHub 上的專案' --mode faithful
voicetype-control process '呃，我想明天再討論。' --mode clean
```

`--mode off` 僅停用模型，仍保留繁化與已學會的詞彙規則。服務預設 CPU 四執行緒，明確使用 `--device none --no-op-offload -ngl 0`。模型閒置兩分鐘後由 llama.cpp 卸載；下一次使用需重新載入。逾時預設 2500 ms，並非每句新增固定 2500 ms。

環境設定：`VOICETYPE_REFINER_URL=http://127.0.0.1:18765` 啟用，`VOICETYPE_REFINER_MODE=faithful|clean`，`VOICETYPE_REFINER_TIMEOUT_MS=2500`。只接受 loopback HTTP；沒有設定 URL 就不啟用模型。

## 安裝與重現

先建置新版 daemon 與 addon，正常安裝 daemon，再安裝使用者 addon：

```bash
cargo build --release --manifest-path voicetyped/Cargo.toml --features sensevoice
cmake -S voicetype-fcitx5 -B voicetype-fcitx5/build \
  -DCMAKE_BUILD_TYPE=RelWithDebInfo -DCMAKE_INSTALL_PREFIX=/usr
cmake --build voicetype-fcitx5/build -j4
ctest --test-dir voicetype-fcitx5/build --output-on-failure
./scripts/install.sh
./scripts/install-addon-user.sh
fcitx5 -rd
```

使用者 addon 裝到 `~/.local/lib/fcitx5`，使用者設定中的 Library 指向完整路徑（省略 `.so`），不需要 sudo。設定優先於系統安裝版本。`install.sh` 也安裝 `voicetype-control` 和 `voicetype-context`。

可選的文字模型：這次使用官方 Qwen GGUF revision `23749fefcc72300e3a2ad315e1317431b06b590a`，檔案 `Qwen3-0.6B-Q8_0.gguf`，639,446,688 bytes；llama.cpp revision `7ab4ee7`。下載與建置是一次性網路操作，日常辨識全本機。

```bash
git clone https://github.com/ggml-org/llama.cpp /tmp/voicetype-llama-build
git -C /tmp/voicetype-llama-build checkout 7ab4ee7
cmake -S /tmp/voicetype-llama-build -B /tmp/voicetype-llama-build/build \
  -DCMAKE_BUILD_TYPE=Release -DGGML_CUDA=OFF -DGGML_VULKAN=ON \
  -DLLAMA_CURL=OFF -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF
cmake --build /tmp/voicetype-llama-build/build --target llama-server -j4
mkdir -p ~/.local/share/voicetype/models
curl -fL -o ~/.local/share/voicetype/models/Qwen3-0.6B-Q8_0.gguf \
  https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/23749fefcc72300e3a2ad315e1317431b06b590a/Qwen3-0.6B-Q8_0.gguf
./scripts/install-refiner.sh /tmp/voicetype-llama-build/build/bin
```

AMD 內顯需要相容 Vulkan 驅動，CPU 模式不需要它；若只建 CPU 可設 `GGML_VULKAN=OFF`。不要把裝置改成自動選擇而意外使用訓練中的獨顯。

停用文字模型、保留學習功能：

```bash
systemctl --user disable --now voicetype-refiner.service
rm ~/.config/systemd/user/voicetyped.service.d/refiner.conf
systemctl --user daemon-reload
systemctl --user restart voicetyped
```

## 驗證界線

`eval/evaluate_context.py` 是 20 個合成**文字**開發案例，分開計修對、漏改和誤改。它不測錄音，不代表 ASR CER，也不是獨立保留測試集。請使用空的學習檔和沒有手動上下文的新 daemon：

```bash
VOICETYPE_SOCKET=/tmp/voicetype-eval/ipc.sock \
VOICETYPE_LEARNING_FILE=/tmp/voicetype-eval/learning.json \
VOICETYPE_REFINER_URL=http://127.0.0.1:18765 \
  voicetyped/target/release/voicetyped
# 另一個終端
python3 eval/evaluate_context.py --socket /tmp/voicetype-eval/ipc.sock \
  --require-fixes 1 --json /tmp/voicetype-context-results.json
```

自己的聲音 TTS 合成、ASR 微調與真實錄音驗收另見 [SYNTHETIC-EVALUATION.md](SYNTHETIC-EVALUATION.md)。本次沒有克隆聲音或訓練 ASR。
