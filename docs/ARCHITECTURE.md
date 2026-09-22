# 架構

VoiceType 是 Fcitx5 的語音聽寫模組：按住熱鍵說話，放開後文字出現在
游標處。全本機推論，預設使用 CPU。

這份文件記錄**為什麼這樣蓋**，而不只是有什麼。每個看起來繞路的決定
背後通常有一次實測。

---

## 全景

```
┌──────────────────────────┐        ┌─────────────────────────────┐
│  fcitx5 (輸入法行程)      │        │  voicetyped (systemd user)  │
│                          │        │                             │
│  libvoicetype.so         │  IPC   │  ┌───────────────────────┐  │
│   ├ 攔 Ctrl+Alt          │ ◄────► │  │ session 狀態機         │  │
│   ├ 鎖定目標 InputContext │ Unix   │  └──────────┬────────────┘  │
│   └ commitString()       │ socket │             │               │
│                          │ NDJSON │  ┌──────────▼────────────┐  │
└──────────────────────────┘        │  │ 音訊擷取 (cpal)        │  │
                                    │  │  ring buffer + preroll │  │
                                    │  └──────────┬────────────┘  │
                                    │  ┌──────────▼────────────┐  │
                                    │  │ 重採樣 → 16kHz         │  │
                                    │  └──────────┬────────────┘  │
                                    │  ┌──────────▼────────────┐  │
                                    │  │ Silero VAD             │  │
                                    │  └──────────┬────────────┘  │
                                    │  ┌──────────▼────────────┐  │
                                    │  │ SenseVoice (FFI shim)  │  │
                                    │  └──────────┬────────────┘  │
                                    │  ┌──────────▼────────────┐  │
                                    │  │ 後處理: 標籤/繁化/詞彙  │  │
                                    │  └───────────────────────┘  │
                                    └─────────────────────────────┘
```

---

## 為什麼是兩個行程

ASR 崩潰不能拖垮輸入法 —— 輸入法掛掉等於整個桌面無法打字。這是唯一
真正的理由，其餘（不阻塞事件迴圈、資源可用 systemd 限制）是附帶好處。

代價是要維護一個 IPC 協定，以及「結果回來時目標視窗可能已經不在」
這個狀態問題。兩邊都要防守：daemon 送出前比對最新的 session id，
addon 收到後也比對自己的。

addon 處理攔鍵、鎖定 InputContext、`commitString()`，以及只有輸入法能觀察的周邊文字／修正歸屬。模型、詞彙規則與持久化放在 daemon。

### 個人化管線（2026-09-22）

繁化 → 靜態詞表 → `Assistant` 個人規則 → 可選 `Refiner` → 再確認 session → 送字。
`personalization.rs` 管理小型、可刪除的修正規則；`assistant.rs` 負責最近送字的歸屬、暫時上下文和模型協調；`refine.rs` 僅與 loopback 的文字模型溝通，逾時或不合規就保留輸入文字。詳細行為與操作見 [PERSONALIZATION.md](PERSONALIZATION.md)。

addon 的 `CorrectionTracker` 只追蹤自己的 commit，必須等應用程式回報相同文字後才開始觀察。周邊文字被可靠地修正後，在下一次 PTT 送出完整 before/after；daemon 再核對 session、IC、program、五分鐘期限和先前送出的原文，才交給詞彙學習。兩次獨立觀察才啟用；選取修正句加 Ctrl+Caps Lock 是明確確認入口。

X11／AT-SPI helper 在開始錄音時執行一次，與音訊擷取並行，500 ms 硬期限。畫面上下文只在記憶體裡；讀不到不會阻止辨識。未確認的歷史 ASR 不會被再次當成人名修正依據。控制用短連線不會觸發取消錄音；只有曾發出 Start 的 addon 連線斷線會取消。

---

## addon（C++，`voicetype-fcitx5/`）

**攔鍵在 `PreInputMethod` 階段。** 晚一點就會被注音吃掉。

**熱鍵是純 modifier 組合（Ctrl+Alt）**，無法用 `fcitx::Key::check()`
表達 —— 那是為「modifier + 一般鍵」設計的。純 modifier 沒有主鍵：
實際送出的事件是**後按下的那個 modifier 自己**（keysym = `Alt_L`），
前一個只出現在 state 裡，而使用者按的順序不固定，兩種順序都得認。
放開時任一個 modifier 鬆開就送出。

取捨：`Ctrl+Alt` 是許多桌面快捷鍵的前綴，誤觸率比 `Ctrl+Alt+Space` 高。
代價由 daemon 的 VAD 吸收 —— 沒有語音就不會有東西被 commit。

**`TrackableObjectReference` 鎖定目標 InputContext。** 錄音期間切換
視窗不會寫進新視窗；IC 被銷毀時弱參考自動失效，避免 use-after-free。

**socket fd 掛進 fcitx5 的 EventLoop，不開執行緒。** 回呼在主執行緒
執行，可以安全呼叫 `commitString()`。

**密碼欄位在 addon 端就擋掉** —— 麥克風根本不會啟動，音訊不會離開這個
決策點。`is_password` flag 仍會傳給 daemon 作為縱深防禦。

**daemon 不可用時不攔截熱鍵**，直接放行給輸入法。使用者不會感覺到
按鍵失效。

---

## IPC

`$XDG_RUNTIME_DIR/voicetype/ipc.sock`，權限 0600，NDJSON（一行一個
JSON 物件）。

選 runtime dir 而非 `~/.local/share`：登出即清除，不會殘留 stale
socket 到下次開機；目錄本身就是 0700。

訊息：`start` / `stop` / `cancel` / `ping` → `result` / `error` /
`pong`。錯誤帶結構化的 `code`（`empty_result`、`too_long`、
`no_audio_device`…），讓 addon 能區分「沒說話」與「引擎掛了」。

daemon 拒絕搶佔已被佔用的 socket。兩個 daemon 搶同一個 socket，
addon 連到誰是未定義的。

---

## 音訊管線

```
麥克風 → PipeWire → cpal → ring buffer → 重採樣 → VAD → ASR
                             (61s 容量)   (→16kHz)
```

**Ring buffer 是 61 秒而不是 30 秒。** 單次錄音上限是 60 秒，30 秒的
緩衝會把一段 45 秒的錄音前半覆蓋掉 —— 使用者拿到的是後半段殘句，
而且沒有任何錯誤提示。緩衝區必須至少能容納「上限 + pre-roll」。

代價是記憶體：48kHz 原生取樣率下約 11.7MB。要壓回去得在音訊回呼裡
就重採樣，那需要在即時執行緒維護重採樣器狀態，不划算。

**500ms pre-roll。** 按下熱鍵到音訊串流真正開始有 50–100ms 的落差，
冷啟動會吃掉第一個字。實測：hold 3.0 秒時冷啟動擷取到 2.95 秒，
暖機後是 3.50 秒 —— 回溯確實補回了串流啟動前被吃掉的開頭。

**串流預設 `warm` 模式**：最後一次使用後保持開啟 30 秒。這代表麥克風
指示燈會持續亮著，是誠實的取捨而不是疏忽 —— README 明確說明。音訊
只存在於記憶體環形緩衝區，不寫檔、不外送。

**重採樣用 sinc 而非抽取。** 沒有低通的抽取會 aliasing，直接傷 ASR
準確度。release 建置下 48k→16k 只花 4ms。

**VAD 在重採樣之後。** Silero 的窗長（512 samples）是以 16kHz 定義的，
在原生取樣率上跑等於改變窗的時間長度。

---

## VAD：正確性需求，不是延遲優化

**沒有 VAD 時，誤觸熱鍵會 commit 幻覺文字。** 對著靜音錄 3 秒，引擎
輸出「我.」或「그.」而不是空字串 —— 內容隨機，但一定會吐出東西。
非自迴歸的 CTC 解碼沒有「什麼都不輸出」這個自然出口，靜音會被硬解成
最像的 token。

這個定位差別不只是措辭：當成延遲優化，VAD 可以延後、可以砍；當成
正確性需求，它是「誤觸熱鍵不會被插入亂碼」的唯一防線。

**權重就在 SenseVoice 的 GGUF 裡**（`_model.stft.*`、`_model.encoder.*`），
不需要第二個模型檔，也不需要第二份 backend。

**分層**：FFI shim 只回傳逐窗語音機率（每 512 samples 一個），門檻、
遲滯與前後留白全在 Rust 端。判定邏輯因此不需要 291MB 的模型就能測，
而 unsafe 邊界上的程式碼維持最少。

**分段策略與上游不同。** 上游 `main.cc` 把錄音切成多個語音片段分別
辨識、丟掉片段間的靜音。PTT 不能這樣做 —— 使用者按住熱鍵時句中的
停頓（思考、換氣）是說話的一部分，切開會讓上下文斷裂。這裡只修剪
首尾，句中一律保留。實測這個差異對品質是正面的。

**遲滯只決定邊界，不決定存在性。** 這裡踩過一次坑：原本 `min_speech`
數的是遲滯後的 active 窗數，結果安靜房間的底噪被判成語音 —— 實測
4 秒環境音只有 7/125 窗超過 0.5，但底噪的 p90 是 0.407，落在
`neg_threshold`(0.35) 與 `threshold`(0.5) 之間。一個偶發尖峰觸發後，
遲滯把後面幾十個窗全部算成語音。

修正後：存在性只數真正超過 `threshold` 的強證據窗，邊界仍用遲滯決定。

| | 強證據窗數 | 判定 |
|---|---|---|
| 4 秒環境音 | 7/125 (6%) | 無語音 |
| 真實語音 | 105/208 (50%) | 有語音 |

15 倍差距，門檻放在中間很安全。全語料 CER 完全沒變。

`--vad-report` 可以印出任何 WAV 的機率分布與判定，調參前先看得到。

---

## ASR 引擎

**SenseVoice-Small，int8 量化，CPU 推論。** 非自迴歸架構，實測 RTF
0.036（約 28× realtime）。GPU 刻意不用（`GGML_CUDA=OFF`）—— GPU 要
留給其他工作負載，這不是效能取捨而是硬性約束。

**q8 相對 fp16 零損失**：mixed 兩者都是 19.6%，zh_pure 都是 4.9%，
省下 156MB。

**為什麼需要 FFI shim**（`voicetyped/shim/`）：SenseVoice.cpp 的公開
介面是 C++ 而非 C —— `sense_voice_full_parallel()` 接
`std::vector<double>&` 且帶預設參數，沒有 `extern "C"`，Rust 無法直接
bindgen。而且它**沒有回傳文字的 API**：結果留在 `ctx->state->ids`
（token id 序列），只能透過 `sense_voice_print_output()` 印到 stdout。
shim 複製那段 CTC 去重邏輯，改成寫進呼叫端的緩衝區。

不走子行程方案的理由：每次錄音重啟 CLI 要重新載入 291MB 模型。

**引擎放在 cargo feature 後面**，讓沒有 third_party 的環境（CI、只改
IPC 的開發循環）仍能建置與跑單元測試。但兩種配置產出**同一個**
`target/release/voicetyped` —— 跑一次不帶 feature 的 build 就會覆蓋掉。
覆蓋後的 daemon 一切正常，只是辨識結果變成 `[null asr: 3.48s]`。
為此不含引擎的建置會拒絕啟動。

**啟動時預熱。** 引擎不用 mmap，291MB 權重要 page fault 進來，各層
計算緩衝要配置 —— 全落在第一次推論上：798ms，是之後每次 190ms 的
四倍。啟動時跑一次丟棄的 1 秒靜音推論（64ms）把成本挪到開機時。

副作用是啟動後 RSS 立刻是 329MB 而不是好看的 198MB。那個 198MB 是
假數字，它只代表模型還沒被真正用起來。

---

## 後處理

```
① 標籤剝除 → ② 繁化 → ③ 詞彙修正 → ④ 贅字移除 → ⑤ 標點 → ⑥ 安全檢查
     ✅          ✅         ✅          未做      未做     未做
```

**① 標籤剝除**：SenseVoice 輸出帶結構化標籤
（`<|zh|><|NEUTRAL|><|Speech|><|withitn|>`），不剝掉會直接出現在使用者
的文字裡。語言標籤保留到這一層才丟，因為它是 per-app profile 的判斷
依據。

**② 繁化**：引擎輸出簡體，目標是繁體台灣用語。用 OpenCC 的 `s2twp`
而不是 `s2t` —— 前者連詞彙一起轉（软件→軟體、默认→預設），後者只轉
字形，會留下「軟件」這種看得懂但不對的詞。

不做成 cargo feature：OpenCC 是一行 apt，而輸出繁體是產品目標不是選配。
build.rs 找不到就直接報錯並給出各發行版的安裝指令。

Ubuntu 的 `libopencc1.1`（runtime）有 `libopencc.so.1.1` 但沒有連結器
要的 `libopencc.so` symlink（那在 `-dev` 裡）。build.rs 因此在 OUT_DIR
補一個 symlink，比讓 cargo 傳 `-l:libopencc.so.1.1.7` 穩健。

**③ 詞彙修正表**：`~/.config/voicetype/vocab.toml`，使用者可編輯。

**範圍刻意收窄。** 修正表的前提是錯誤穩定且可枚舉，而實測的中英夾雜
錯誤大多不是 —— 同一個詞每次錯得不一樣（`commit`→「抗 make」、
`rebase`→`re`、`fallback`→`feedback back`）。更要緊的是很多錯誤結果
是**合法詞**：`cache` 常被聽成 `catch`，但加一條 `catch`→`cache` 會讓
「try catch」變成「try cache」—— 修正表沒有語境。

所以只收兩種：音譯成不合法中文詞的技術詞（「熱力瑞」→ `learning rate`），
加上使用者自己的專案詞彙。原則：**寧可漏掉，不可誤改。**

實作是**單次由左至右掃描、每個位置取最長匹配**，不是「對每條規則各跑
一次 replace」—— 那樣規則之間會互相破壞：`learning rate` 修好之後
`rate` 那條又會把它咬掉一半。

---

## Session 狀態機

```
Idle → Recording → Transcribing → Delivering
```

所有 session 命令由**單一** task 依序處理，順序因此天然成立，不需要
為 start/stop 的交錯設計鎖。真正耗時的兩件事各自離開這條序列：

- 音訊操作走 `spawn_blocking` 並 `await`，仍在序列內；
- ASR 推論丟到獨立 task，**不** await —— 否則使用者在推論期間按下的
  下一次 PTT 會被卡住。

代價是結果可能在下一個 session 已經開始後才回來。這是「過期結果」的
來源，兩端都比對 session id。

---

## 資源特性

| 階段 | RSS | CPU |
|---|---|---|
| 閒置（預熱後） | 329 MB | 0.00 核 |
| 錄音中 | 336 MB | 0.01 核 |
| 推論中 | 359 MB（尖峰 372） | 3.19 核（尖峰 4.19） |

**錄音本身幾乎不耗 CPU。** 成本全在推論那 ~200ms，而且確實是 4 threads
（3.19 核平均對上設定的 4）。執行緒數刻意限制，避免與編譯工作互搶。

**推論不洩漏記憶體。** 同一行程連續推論 12 次穩定在 378.5MB，第一次
的 +68.7MB 之後完全平坦。

但 daemon 用一陣子後會到 494MB。差額是 glibc 的 per-thread arena：
tokio 的多執行緒 runtime 讓配置散在十幾個 arena，每個保留自己的空閒
頁不還給 OS。PTT 正好是 glibc 假設最不成立的模式 —— 每次爆發配置
幾十 MB，然後閒置好幾分鐘。

兩個手段：service 設 `MALLOC_ARENA_MAX=2`，推論結束後呼叫
`malloc_trim(0)`。

`scripts/measure-resources.py` 對 daemon 的 PID 做 20Hz 取樣，同時經
IPC 驅動一次完整循環 —— 推論只有 200ms，`systemctl status` 的單次
數字看到尖峰的機率很低。

---

## systemd

user service，開機自動啟動。加固選項逐項在 user manager 下實測過。

**`Type=simple` 而不是 `notify`**：daemon 沒有實作 `sd_notify`，
notify 會讓 systemd 等一個永遠不會來的就緒訊號，90 秒後判定啟動失敗。

**不設 `AllowedCPUs`**：寫死的核心編號在核心數較少的機器上會綁到不存在
的 CPU，systemd 直接拒絕啟動。真正限制 CPU 用量的是推論的執行緒數。

**不設 `ProtectKernelModules`**：它要調整 capability bounding set，而
systemd 的 **user** manager 沒有那個權限 —— 加了會得到
`status=218/CAPABILITIES`。

---

## 評測

`eval/evaluate.py`，CER 以字元編輯距離計算。

**`--engine-type` 決定量的是誰。** `daemon*` 走 daemon 自己的
`--transcribe`，也就是產品實際跑的管線；`sensevoice` 走上游的
`sense-voice-main` CLI。兩者不是同一條路徑（CLI 多段切分後 join，
daemon 整句解碼），分數差距可達 9 個百分點，方向一致地**低估產品**。

這是量測可信度的核心問題：拿 CLI 的分數當產品的分數是類別錯誤。

**正規化必須做繁簡摺疊。** 少了它，一句完全辨識正確的中文

```
參考  明天下午三點我們開會討論這件事情
辨識  明天下午三点我们开会讨论这件事情
```

會被算成 43.8% CER。工具在缺 OpenCC 時直接中止而不是靜默給數字。

**ITN 是同一類問題。** 產品開著 ITN（「三十二」→ 32，聽寫想要的形式），
但參考文本寫中文數字，於是「明天下午3点」被算成錯誤 —— 它一個字都
沒聽錯。`daemon` / `daemon-novad` 預設關掉 ITN；`daemon-itn` 才是
使用者實際看到的輸出，但那組分數不可與其他並列。

轉換方向選繁→簡（t2s）而非簡→繁：t2s 是多對一的確定性映射；s2t 有
一簡對多繁的情況，轉換本身就會產生誤差，那個誤差會被誤算成辨識錯誤。

---

## 反覆出現的失敗模式

這個專案被同一種問題咬過很多次，值得單獨記一節：**每一步都回報成功，
但做到／量到的不是你以為的東西。**

- 評測工具漏掉繁簡摺疊 → 完全正確的中文報 43.8% CER，整組虛報 36.2%
- 評測量的是上游 CLI 而不是產品 → 英文 WER 虛報 9 個百分點
- ITN 讓表示差異被算成辨識錯誤 → zh_pure 從 4.4% 虛報到 6.2%
- addon 的 `.conf` 沒被產生 → make 印「Generating」、回傳成功、沒有檔案
- addon 裝到 fcitx5 不掃描的目錄 → `cmake --install` 回報成功
- daemon 跑的是佔位引擎 → 熱鍵有反應、log 有 transcribed、文字也出現，
  只是內容是 `[null asr: 3.48s]`
- 安裝腳本的 `strings | grep -q` 在 `pipefail` 下永遠回報失敗
- VAD 的遲滯污染存在性判斷 → 安靜房間被判成有語音
- 「閒置 RSS 198MB」→ 那只代表模型還沒被用起來

共同點不是粗心，是**沒有任何一步報錯**。防線因此一律傾向「不確定就
中止」：缺 OpenCC 中止、安裝前綴不對中止、沒有引擎中止、有別的 daemon
佔著 socket 中止。

第二個模式是**合成的測試訊號比真實世界乾淨**。VAD 用 −54dBFS 的合成
噪音驗證過，但真實房間的底噪有結構，p90 落在遲滯的兩個門檻之間 ——
那是合成訊號測不出來的。

---

## 目錄

```
voicetype-fcitx5/     fcitx5 addon (C++，極薄)
voicetyped/           ASR daemon (Rust)
  shim/               SenseVoice.cpp 的 C 介面
  src/audio/          擷取、ring buffer、重採樣
  src/vad/            Silero VAD 的判定邏輯
  src/asr/            引擎抽象與 FFI
  src/postproc/       標籤剝除、繁化、詞彙修正
  src/session.rs      狀態機
  src/ipc.rs          Unix socket + NDJSON
eval/                 語料與 CER/WER 評測
config/               預設詞彙表
systemd/              user service unit
scripts/              安裝、資源量測、VAD 診斷
```
