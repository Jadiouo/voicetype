# 快速詞庫與選配 CPU 校字

本功能在 ASR 後自動修正明確錯字，不顯示逐句確認，不生成整句、不刪除重複。預設只有本機詞庫；CPU 模型必須另外安裝並設定 socket 才會啟用。

## 詞庫與名字

`~/.config/voicetype/vocab.toml` 儲存後下一句自動重載。格式無效時沿用上一份有效詞庫；刪除檔案則清空。大小寫不敏感、英文有詞界、取最長單次匹配，保留反引號程式碼、路徑及較長識別字。

```toml
# 放在第一個 [[entry]] 前面。這些是明確指定的正確字形。
names = ["GitHub", "游錫堃"]

[[entry]]
wrong = ["gthub", "git hub"]
right = "GitHub"
```

每個名字 2–64 字、最多 256 個。完整名字在前後兩次 OpenCC 轉換、別名、既有個人學習規則及 CPU 模型階段都受保護。短別名不能改長名字內部，也不能跨越名字邊界。唯一的 OpenCC 字形變體可還原成指定名字；兩個不同已配置名字不互相合併。一次請求使用同一份詞庫快照。

名字依指定大小寫精確匹配，例如指定 `Mabe` 可保留人名，同句小寫 `mabe` 仍可由原有明確規則修正。未知名字不會自動加入清單，原有學習規則也不會被刪除。不要將兩個合法但意思不同的詞加入無條件別名，例如 `catch → cache`。

## CPU 模型的行為

流程為繁體轉換 → 詞庫 → 個人規則 → 選配 CPU 校字 → 繁體及輸出檢查。使用 OpenCC `s2tw` 保留原用詞，避免 `s2twp` 的地區詞彙替換；缺少 OpenCC 或轉換失敗時不送出未驗證文字。錄音輸出與 `ProcessText` 共用此邊界，結果仍拒絕含日文假名、韓文或注音的輸出；Latin 字母不受此限制。

CPU 校字器以簡體影子字串推論，只回傳原文位置的單漢字替換。候選機率至少 0.98、原字機率最多 0.01，且兩字須同音節（忽略聲調）。數字、否定、人稱代名詞、買／賣、已知詞庫詞、稱謂前姓名、引文、反引號程式碼及路徑不由模型改寫。Rust 端重新驗證位置與保護條件，保留其他所有字元。這些保護是模型的限制；既有手動詞庫／學習規則仍有各自語義。

模型不是通用潤稿器；英文原字不經模型改寫。英文拼字依明確詞庫，漏字、多字、不同音節、低置信錯字或未知專名可能不修。最多 1024 字，96 字核心分塊帶 16 字上下文，80ms 模型預算；超時放棄整個模型階段，不送出部分修正。Rust 給整次 IPC 100ms 等待預算，牆鐘時間仍受作業系統計時及排程影響，並非硬即時保證。

`VOICETYPE_CSC_SOCKET` 未設定時不啟用。設定後使用本機 Unix socket，模型失敗就保留進入模型前的文字，不呼叫舊生成式 refiner 補救。`ProcessText mode=off` 繞過模型，但仍執行繁體、詞庫和個人規則。無錄音、桌面截圖或網路資料會由校字器自行取得；日誌只記狀態、數量與時間。

## 模型準備與安裝

來源為作者的 [MacBERT4CSC ONNX 模型](https://huggingface.co/shibing624/macbert4csc-base-chinese/tree/615e6e09ef9a69ec487bc7c641ec3a311e2c11b9)，固定 revision `615e6e09ef9a69ec487bc7c641ec3a311e2c11b9`，模型標示 Apache-2.0。權重不包含在此 repository。準備階段需要網路下載固定檔案，執行階段完全使用本機檔案。

```bash
python3 -m venv private/csc-build-venv
private/csc-build-venv/bin/pip install -r config/csc-requirements.txt onnx==1.20.1
private/csc-build-venv/bin/python scripts/prepare_csc.py --output-dir private/csc-prepared
```

腳本先核對作者 ONNX／tokenizer 雜湊，再做 CPU 圖融合及動態 INT8 量化，拒絕覆寫既有量化模型。固定工具版本的重建模型 SHA256 為 `38a1bcad77a183e3a229c839c192a503f730ac8871f611fd698c6bb22575966d`；來源、版本與雜湊記錄在 `prepared.json`。常駐 runtime 不需要 torch 或建置用 onnx 套件。

先依 README 建置並更新 daemon，再準備獨立 runtime。以下範例使用一個新的版本目錄；升級既有安裝時先保留舊目錄與原 service 設定。

```bash
csc_runtime="$HOME/.local/lib/voicetype/csc-v1"
csc_models="$HOME/.local/share/voicetype/models/csc-macbert-615e6e"
install -d "$csc_runtime" "$csc_models"
python3 -m venv "$csc_runtime/venv"
"$csc_runtime/venv/bin/pip" install -r config/csc-requirements.txt
install -m 644 scripts/voicetype_csc.py "$csc_runtime/voicetype_csc.py"
install -m 644 private/csc-prepared/model-int8-fused.onnx private/csc-prepared/tokenizer.json private/csc-prepared/prepared.json "$csc_models/"
ln -s "$csc_runtime" "$HOME/.local/lib/voicetype/csc-current"
install -d "$HOME/.config/systemd/user"
install -m 644 config/voicetype-csc.service "$HOME/.config/systemd/user/voicetype-csc.service"
systemctl --user daemon-reload
systemctl --user start voicetype-csc.service
```

範本明確指定 CPUExecutionProvider、CPU wheel、8 執行緒，關閉空閒自旋，記憶體上限 768MiB、無 swap，限制通訊為 AF_UNIX。命令列也接受 1／2／4／8 執行緒，可按 CPU 負載調整；腳本預設 4。worker 與 ASR 分程序，不改 ASR 使用的推論函式庫。模型及 tokenizer 必須符合範本雜湊，載入並暖機成功才建立私人 socket（目錄 0700、socket 0600）。

用 `systemctl --user edit voicetyped.service` 加入：

```ini
[Unit]
Wants=voicetype-csc.service
After=voicetype-csc.service

[Service]
Environment=VOICETYPE_CSC_SOCKET=%t/voicetype-csc/worker.sock
```

然後 `systemctl --user restart voicetyped.service`。以 `systemctl --user show voicetype-csc.service -p ActiveState -p SubState -p NRestarts` 確認 active/running，並用 `python3 scripts/voicetype-control.py process '今天新情很好。'` 測試實際文字路徑；命令不會錄音或插字。服務起來不等於實際套用了模型，應核對結果是否變為「今天心情很好。」。

停用時移除自己新增的 Wants/After/socket 設定，重新載入並重啟 daemon，再停止 worker；保留其他既有 drop-in。暫時只停 worker 也會立即回退原文，但下一次 daemon 啟動會因 Wants 重新啟動它。

## 驗證界線

`cargo test --manifest-path voicetyped/Cargo.toml` 涵蓋詞庫重載、無效檔案、名字碰撞、個人規則優先權、位置協定、失敗與期限。Python 無權重測試使用 `private/csc-build-venv/bin/python -m unittest discover -s eval -p 'test_csc_*.py'`，涵蓋政策與真正 Unix socket 協定、畸形要求、過期要求、斷線及日誌隱私。

有模型時可用 `eval/check_csc.py` 與 `eval/fixtures/csc-smoke.json` 跑完整文字 IPC。這些是合成迴歸案例，不能宣稱整體語音辨識率改善；文字 IPC 時間也不是停止錄音至送字的完整等待。模型仍可能漏修，陌生專名尤其需要明確詞庫。採用與否應以自己的句子、錯改情形和實際等待時間判斷。
