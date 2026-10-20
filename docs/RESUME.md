# 目前工作續接入口

> 本頁只放「現在的狀態」。舊歷史（2026-10-06 至 10-11 的逐輪紀錄）原封保存在
> `private/resume-history/RESUME-2026-10-11-full.md`（僅本機，不公開）。
> 舊歷史裡的待辦（Google PCM／audio review、APP-05、preview 重跑、雙辨識 goal 等）
> 已被 2026-10-11 的最新指令取代，不要沿用。

## 目前狀態（2026-10-11 最新指令）

- 方向：收斂成 local-only 版本，合併 main，清理儲存空間。
- PR #2（`feat/desktop-app`）已標準 merge，merge commit `a61ef65`。main 已 fast-forward 到此。
- Windows／Linux App 的 Google CLI 功能已由使用者取消。啟動與 reload 時，舊的 Google 選擇會被正規化成 Local。保留舊快捷鍵交接的明確還原入口。
- 舊的雙辨識 goal 已暫停（paused）。**不要自動恢復 Google 或 Windows TSF 相關工作**，除非使用者明確要求。
- 儲存清理已完成：38 項可再生快取／重複產物，加上已合併 App 工作樹的正常移除。未使用 force，未刪任何 branch。

## 尚未完成（2026-10-15 更正，不要宣稱完整版完成）

1. **Windows 自動送字 v1 已實作並合併**（PR #4，2026-10-15，main a5b99ab）：模擬鍵入送進聚焦欄位，失敗保留全文。已知限制見 `desktop/README.md`（不偵測輸入法組字、瀏覽器內換欄位偵測不到）。尚待使用者 Win11 實機驗收。
2. **main 的 preview CI 已修復**（2026-10-15 合併 #6、#5，main f8c1f85）：過期 UI 測試、Nano 在 AVX2-only CPU 回空字串（ORT `session.x64quantprecision=1`）、fcitx 測試 TMPDIR 前提。自動送字 PR #4 已於 2026-10-15 合併。
3. **Linux 日常版已部署 main df84d23（2026-10-20）**：校正補字／刪字可加入詞庫、句首句尾錨定。分支只剩 main。
4. **等使用者 Win11 實機驗收**：安裝包與驗收清單在 GitHub prerelease `v0.2.1-alpha.1-preview`（main a5b99ab）。收到回報後對照 goal 完成標準判定；當日完整紀錄見 `private/devlog.md` 2026-10-15。

## 保留中的未 merge worktree

位置都在 `.claude/worktrees/` 下。尚未 merge，也可能有未提交內容，**不要自己刪、不要 force**：

| 路徑 | branch |
| --- | --- |
| `.claude/worktrees/adaptive-core` | `feat/adaptive-core` |
| `.claude/worktrees/fast-autocorrect` | `feat/fast-autocorrect` |
| `.claude/worktrees/google-app-integration` | `feat/google-app-integration-work` |
| `.claude/worktrees/google-key-coexistence` | `feat/google-key-coexistence` |
| `.claude/worktrees/windows-dictation` | `feat/windows-dictation-work` |

各分支的評估與去留建議：`private/branch-review.md`。

## 不可做的事

- stash `18f9ce552c422ebb600ba531dca4be727df9af72` 含私有內容：**不要 pop、drop、push**。
- 不要宣稱完整版完成、不要宣稱真人 ASR 品質普遍提升（沒有這類證據）。
- 不要把私人日誌、個人詞庫、錄音、驗證資料、絕對路徑放進公開文件或推到 GitHub。
- 不要 push 私人的 `feat/fast-autocorrect` 分支（含私人歷史）。
- 不要改日常服務、日常 venv／模型、麥克風設定與使用者學習檔。

## 重要證據位置（都只在本機）

- 完整舊歷史：`private/resume-history/RESUME-2026-10-11-full.md`
- 儲存清理：`private/storage-cleanup/`（`manifest.md`、`deleted.jsonl`）
- 合併前 root 的 14 個檔案備份與恢復說明：`private/main-consolidation-backup/RECOVERY.md`
- 已移除的 App 工作樹私有交接（571 檔）：`private/desktop-app-retained/`（見 `MANIFEST.md`、`REMOVAL-RUNBOOK.md`）
- 更早的私有決策與驗證：`private/devlog.md`、`private/validation/`

## 仍有效的注意事項（括號為舊歷史完整檔的行號）

- GPU 工作一律走 `gpujob` 排隊，不直接跑；日常 CSC 校字是 CPU only（CPUExecutionProvider），不用 Torch／CUDA。（L374、L42、L55）
- worktree 規則：放在 repo 內 `.claude/worktrees/`，暫存放專案內 `.scratch/`；清理前先 `git fetch`，再用 `merge-base --is-ancestor` 或已 merge 的 PR 核對。已 merge 的才移除，不 force、不刪 branch；未 merge 或有未提交修改的先留著。（L386、L440）
- 本機錄音快捷鍵是 Ctrl+Alt，Esc 取消；學習快捷鍵與歷史 Google 快捷鍵不要混淆。（L300）
- 偶發的「句尾 terminal 漏字」仍未定位：一次成功或後處理保留詞尾，都不算修好。有失敗樣本才查；不要先加停錄延遲，也不要放寬校正門檻。（L372、L374）
- CSC 校字有 100ms 等待預算，逾時回退原文。不要為了讓某次診斷過關而延長。（L51、L438）
- 不要刪除使用者的學習檔；英文人名／專名要受保護，不被個人規則改掉。（L55、L59）
- 個人詞庫只留本機。不要用舊基線檔覆蓋使用者較新的 `vocab.toml`。（L67、L73）
- 日常 user services（`voicetyped`、`voicetype-csc`）維持 active，不要因為整理或測試而重啟。（L300、L9）
- 不要把 preview 或測試包當完整產品。CI 通過、預錄音檔轉錄成功，都不等於麥克風、快捷鍵或 TSF 送字完成。（L302、L29）
- 回復 CPU 校字相關部署時，先用較新的 transaction 的 rollback，再用較舊的（路徑見舊歷史 L73、L53、L59）。（推論：是否仍符合目前日常安裝，用前要先核對。）

## 待查證（推論，不確定是否仍有效）

- 學習快捷鍵：舊紀錄 L67／L300 寫法互相矛盾。使用者 2026-10-15 表示實際不用快捷鍵，都是點 Linux 右上角的系統匣圖示做學習；快捷鍵可視為非必要，不要花力氣在它上面。
- 上面「回復交易」指令是 2026-10-06 的部署狀態；之後 Nano 與 GUI 都換過版本，未必還能直接用。
