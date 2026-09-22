# 個人化驗收方法

啟用修正學習與上下文入口；文字模型仍為實驗選項，預設關閉。通過少量開發案例，不代表日常使用有效。

## 軟體整合

- Rust 測試檢查規則保存、兩次獨立修正、完整人名擷取、焦點隔離、過期規則、字面程式碼保護，以及控制連線不取消錄音。
- Fcitx 的 `ipc`、`context`、`lifecycle` 測試使用真正事件派送與 Unix IPC，涵蓋 commit、修正回饋、Ctrl+Caps Lock、焦點切換和敏感欄位。
- 桌面 helper 應以可見的合成測試視窗驗證：讀到一般標籤、排除可編輯／密碼欄位；停用 accessibility bridge 時退回視窗標題。不同瀏覽器的文字介面需要個別驗證。
- 快捷鍵應回報成功、尚待第二次觀察或未學到的原因。不能把「已送出確認」當成「已保存規則」。

## 模型驗收

使用同一份輸入，比較模型關閉與開啟後的結果。逐字正解只能用於計分，不能塞進上下文。分別計算改善、惡化、原本正確卻被誤改，以及暖機後真正呼叫模型的等待時間。跳過模型的案例不應混入模型速度宣稱。

實驗的小模型曾把合法英文詞改成相近技術詞，也無法可靠區分同姓人物的角色。候選限制和 JSON 驗證只能擋掉部分不合規修改，不能保證模型理解語意。這是預設關閉文字模型的原因；修正學習不依賴它。

公開的 `context_cases.json` 與 `context_holdout.json` 都是合成文字範例，沒有真人錄音或個人詞庫。它們不代表 ASR 準確度。使用者自己的錄音、逐字稿、詳細結果和機器量測應保存在忽略的 `private/`、`eval/recordings/` 或其他本機位置。

## 重跑

先按 [PERSONALIZATION.md](PERSONALIZATION.md) 建立獨立、空學習檔的 daemon，並啟動實驗模型服務：

```bash
python3 eval/evaluate_context.py --socket /tmp/voicetype-eval/ipc.sock \
  --require-fixes 1 --json /tmp/context-dev.json
python3 eval/evaluate_context.py --socket /tmp/voicetype-eval/ipc.sock \
  --cases eval/context_holdout.json --json /tmp/context-holdout.json
.venv/bin/python eval/evaluate_postproc.py --input /path/to/asr-baseline.json \
  --socket /tmp/voicetype-eval/ipc.sock --json /tmp/postproc-results.json
```

`evaluate_postproc.py` 回放已保存的 ASR 文字，不會重新解碼錄音。它會分開列出原始辨識、繁化／詞表後，以及模型校正後的分數。

如果針對保留案例修改了模型或提示詞，這些案例就成為開發資料；下一輪必須再收新的真人錄音和未調參案例驗收。聲音克隆與資料增強另見 [SYNTHETIC-EVALUATION.md](SYNTHETIC-EVALUATION.md)。
