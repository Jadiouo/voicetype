# VoiceType

按住熱鍵說話, 放開後文字出現在游標處 —— 不限應用程式, **包含終端機**。

Fcitx5 語音聽寫模組。全本機推論，ASR 使用 CPU；可選的文字校正也預設 CPU。記憶體依使用狀態與是否啟用文字模型而變，不保證低於 400 MB。

架構與設計理由: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)

---

## 用法

| 按鍵 | 動作 |
|---|---|
| 按住 `Ctrl` + `Alt` | 開始錄音, 放開任一鍵即送出 |
| 錄音中按 `Esc` | 取消, 不送出任何文字 |
| 選取完整修正句，按 `Ctrl+Caps Lock` | 立即學習上一句的詞彙修正 |

新版支援觀察修正、個人詞彙、目前視窗上下文及可選的本機文字模型。
操作、限制、停用方式見 [個人化說明](docs/PERSONALIZATION.md)。
自動學習在能確認歸屬時，累積兩次獨立修正才啟用；不會重新訓練 ASR 權重。

`Ctrl+Alt` 是桌面環境許多快捷鍵的前綴（`Ctrl+Alt+T` 開終端、
`Ctrl+Alt+方向鍵` 切工作區）。按住它說話時再碰到其他鍵, 那些快捷鍵
**仍然會觸發** —— modifier 狀態由 X server 維護, 不因為 addon 吞掉
按鍵事件而改變。選它是因為好按; 誤觸的代價由 VAD 吸收: 沒有語音就
不會有東西被送出。

---

## ⚠️ 關於麥克風指示燈

預設的 `warm` 串流模式下, **麥克風在最後一次使用後會保持開啟 30 秒**,
GNOME 右上角的麥克風指示燈在這段期間會持續亮著。

這是為了消除首字延遲: 按下熱鍵到音訊串流真正開始有 50–100ms 的落差,
冷啟動會吃掉你說的第一個字。

**沒有任何音訊在這段期間被儲存或送出。** 音訊只存在於記憶體的環形
緩衝區, 不寫檔案、不外送。若仍不希望指示燈持續亮著, 改用 `strict`
模式 —— 代價是每次都會損失第一個字。

---

## 現況

**M0 完成, M1 進行中** (SDD §9)。

| 項目 | 狀態 |
|---|---|
| Rust daemon + Unix socket IPC | ✅ 端到端打通 |
| 音訊擷取 (cpal + ring buffer + pre-roll) | ✅ 實測 pre-roll 生效 |
| 重採樣至 16kHz | ✅ release 下 4ms |
| SenseVoice 引擎接進 daemon (FFI) | ✅ 端到端 194ms |
| 中英夾雜品質驗證 | ✅ 125 句實測 |
| Silero VAD (空錄音防護 + 首尾修剪) | ✅ 5.4ms, 100 句零誤判 |
| 繁化 §4.6 ② (OpenCC s2twp) | ✅ 繁體台灣用語 |
| 詞彙修正表 §4.6 ③ | ✅ 使用者可編輯, 範圍刻意收窄 (見下) |
| systemd user service | ✅ `scripts/install.sh` 一鍵安裝 |
| fcitx5 addon (攔鍵 / commitString) | ✅ 已在 fcitx5 5.1.7 載入並實測 |
| M0 出場條件 | ✅ 端到端打通 (Chrome / X11) |

### 已量測到的數據

| 項目 | 實測 | SDD 預期 |
|---|---|---|
| **放開熱鍵 → 結果** (含重採樣+推論) | **194 ms** | P50 < 400ms (§1.3) ✅ |
| SenseVoice RTF (CPU, 4 threads) | 0.036 (~28×) | 17–20× (§4.1) |
| 重採樣 48k→16k (release) | 4ms | ~10ms (§5.2) |
| VAD 掃描 (2.56 秒音訊) | 5.4ms (470× realtime) | — |
| 模型常駐 | 291.56 MB | ~250 MB (§6.1) |
| daemon RSS (預熱後) | 329 MB | < 400 MB (§1.3) ✅ |
| 錄音中 CPU | 0.01 核 | 閒置 < 0.5% (§1.3) ✅ |
| 推論中 CPU (平均 / 尖峰) | 3.19 / 4.19 核 | 4 threads (§6.2) ✅ |
| 首次辨識 (無預熱) | 798 ms | P50 < 400ms ❌ → 改用啟動預熱 |
| 中文 CER (`zh_pure`) | **4.6%** | < 12% (§1.3) ✅ |
| 中英夾雜 (`mixed`) | 16.3% | (§8.1 決策點 20%) |
| 英文 WER (`en_pure`) | 18.7% | < 10% (§1.3) ❌ |

準確度數字走 daemon 自己的推論路徑 (`--engine-type daemon`), 評測時
關掉 ITN。早期的數字量的是上游 CLI, 兩者差距不小 (英文 WER
27.1% → 18.7%) —— 理由見 [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) 的「評測」一節。

### 已知的未解問題

1. **英文 WER 18.7% 未達 10% 的目標**。whisper-small 較好但延遲慢
   16 倍 (同一個 6.65 秒音檔: SenseVoice 220ms, whisper 3630ms),
   不能用在 PTT。
2. **句內語言切換時語言判定會崩潰** —— `branch` → `なんか french`、
   填充音「嗯」→ 日文「うん」。詞彙修正表對這類開放集錯誤無效。
3. **評測用關掉 ITN 來迴避數字表示的差異**。長期應該在 `normalize()`
   做中文數字↔阿拉伯數字的摺疊, 就像繁簡那樣。
4. **後處理 §4.6 的 ④–⑥ 還沒做** —— 贅字移除、標點正規化、安全檢查
   (§4.5 送進 shell 的換行會立即執行 —— 唯一有安全後果的一項)。
5. **記憶體要靠兩個外部手段才壓得住**: `MALLOC_ARENA_MAX=2` 與推論後
   的 `malloc_trim`。沒有它們, daemon 用一陣子會到 494MB, 而同樣的
   推論在單執行緒行程裡跑 12 次穩定在 378MB —— 差額是 glibc 的
   per-thread arena 不歸還 OS。長期使用的數字還需要更多觀察。

### 已解決

* **輸出是簡體中文** —— 引擎輸出簡體, §1.1 要的是繁體台灣用語。
  後處理 §4.6 ② 用 OpenCC `s2twp` 補上, 詞彙也一併轉
  (软件→軟體、默认→預設), 不只是字形。
* **誤觸熱鍵會插入幻覺文字** —— 對著靜音錄 3 秒, 引擎會輸出
  「我.」或「그.」而非空結果 (內容隨機, 但一定會吐出東西)。
  M1 的 VAD 擋掉了這件事。SDD §4.4 把 VAD 寫成延遲優化, 它其實是
  正確性需求 (D10)。

---

## 安裝

```bash
./scripts/install.sh
```

裝 daemon 到 `~/.local/bin`、模型到 `~/.local/share/voicetype/models`、
詞彙表到 `~/.config/voicetype/vocab.toml`, 註冊成 systemd user service
(登入後自動啟動)。addon 可用 `./scripts/install-addon-user.sh` 安裝到使用者目錄，
再 `fcitx5 -rd` 重啟，不需要 sudo；詳見個人化說明。

需要先建置過 daemon (見下)。腳本會在執行檔不含 ASR 引擎時直接中止 ——
那種執行檔跑起來一切正常, 只是辨識結果是 `[null asr: 3.48s]`。

```bash
systemctl --user status voicetyped     # 狀態
journalctl --user -u voicetyped -f     # 即時 log
systemctl --user restart voicetyped    # 換了新版本或改了詞彙表之後
```

---

## 詞彙修正表

`~/.config/voicetype/vocab.toml`。改完 `systemctl --user restart voicetyped`。

```toml
[[entry]]
wrong = ["艾薩克心", "isac king"]
right = "Isaac Sim"
```

**這張表的範圍刻意收窄。** 修正表的前提是錯誤穩定且可枚舉, 而實測的
中英夾雜錯誤大多不是 —— 同一個詞每次錯得不一樣 (`commit`→「抗 make」、
`rebase`→`re`、`fallback`→`feedback back`)。

更要緊的是**很多錯誤的結果是合法詞**。`cache` 常被聽成 `catch`, 但
加一條 `catch`→`cache` 會讓你講「try catch」時變成「try cache」——
修正表沒有語境, 分不出哪一次是錯的。

所以只加兩種:

1. **音譯成不合法中文詞** 的技術詞 —— 「熱力瑞」→ `learning rate`。
   沒有人會真的想打「熱力瑞」, 誤傷風險接近零。
2. **你自己的專案詞彙** —— 這才是這張表真正的價值。

原則: **寧可漏掉, 不可誤改。** 漏掉的你補一個字就好; 誤改的你得先
發現它改錯了, 而聽寫出來的東西通常不會逐字重讀。

---

## 建置

### 依賴

```bash
sudo apt install libfcitx5core-dev libfcitx5utils-dev libfcitx5config-dev \
                 fcitx5-modules-dev extra-cmake-modules libopencc-dev \
                 libasound2-dev libnotify-bin pkg-config build-essential
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### daemon

```bash
cd voicetyped
cargo build --release --features sensevoice  # 含引擎 (需先建好 third_party, 見下)
cargo test --features sensevoice
```

引擎放在 feature 後面, 是為了讓沒有 third_party 的環境 (CI、只改 IPC
的開發循環) 仍能建置與跑單元測試。OpenCC 則**不是** feature —— 輸出
繁體是 §1.1 的目標之一, 而它只是一行 apt。缺了會在 build 階段就報錯
並給出安裝指令。

**注意兩種配置產出同一個 `target/release/voicetyped`** —— 跑一次
不帶 feature 的 `cargo build --release` 就會把含引擎的那份覆蓋掉。
覆蓋後的 daemon 一切正常, 只是辨識結果變成 `[null asr: 3.48s]`。
為此不含引擎的建置會拒絕啟動, 除非設 `VOICETYPE_ALLOW_STUB_ENGINE=1`。

執行時的模型路徑預設是
`$XDG_DATA_HOME/voicetype/models/sense-voice-small-q8_0.gguf`,
可用 `VOICETYPE_MODEL` 覆寫。

### fcitx5 addon

```bash
cd voicetype-fcitx5
cmake -B build -DCMAKE_BUILD_TYPE=RelWithDebInfo -DCMAKE_INSTALL_PREFIX=/usr
cmake --build build
ctest --test-dir build
sudo cmake --install build      # → /usr/lib/x86_64-linux-gnu/fcitx5/
fcitx5 -rd                      # 重啟輸入法
```

`-DCMAKE_INSTALL_PREFIX=/usr` 不能省。CMake 預設的 `/usr/local` 會把
addon 裝到 fcitx5 **不會掃描**的目錄, 而且安裝會回報成功 —— 你只會在
按下熱鍵沒反應時才發現。設定檔會在前綴不對時直接中止並給出正確指令。

安裝的是兩個檔案: `libvoicetype.so` 與 `voicetype.conf`。少了 `.conf`
fcitx5 不會載入 module, `.so` 裝好了也一樣。

### ASR 引擎與模型

```bash
git clone --depth 1 https://github.com/lovemefan/SenseVoice.cpp third_party/SenseVoice.cpp

# 靜態建置供 daemon 連結 (避免 ggml .so 的 rpath 問題)
cmake -S third_party/SenseVoice.cpp -B third_party/SenseVoice.cpp/build-static \
      -DCMAKE_BUILD_TYPE=Release -DGGML_CUDA=OFF -DBUILD_SHARED_LIBS=OFF
cmake --build third_party/SenseVoice.cpp/build-static -j

# 動態建置供評測腳本使用 CLI (可選)
cmake -S third_party/SenseVoice.cpp -B third_party/SenseVoice.cpp/build \
      -DCMAKE_BUILD_TYPE=Release -DGGML_CUDA=OFF
cmake --build third_party/SenseVoice.cpp/build -j

mkdir -p models && curl -L -o models/sense-voice-small-q8_0.gguf \
  https://huggingface.co/lovemefan/sense-voice-gguf/resolve/main/sense-voice-small-q8_0.gguf
```

`GGML_CUDA=OFF` 不是省略而是刻意 —— 見 SDD §2/C2: GPU 要留給
Isaac Sim 與訓練工作, ASR 完全跑在 CPU 上。

---

## 驗證

```bash
# daemon 的 IPC 端到端 (不需要 fcitx5)
cargo run --release -p voicetyped &
python3 scripts/smoke-ipc.py --hold 3

# 資源佔用 (§6.1 / §6.2)。--speak 會提示你說話, 否則 VAD 擋下就
# 量不到推論階段。
python3 scripts/measure-resources.py --hold 3 --speak

# VAD 判定的診斷: 印出逐窗機率分布, 看它是險過還是穩過
voicetyped --transcribe some.wav --vad-report

# 語料錄製與評測 (SDD §8.1)
python3 -m venv .venv && .venv/bin/pip install opencc-python-reimplemented
python3 eval/record.py --list
python3 eval/record.py mixed

# 產品的推論路徑 (預設關 ITN, 見下)
.venv/bin/python eval/evaluate.py --engine-type daemon --set mixed -v
.venv/bin/python eval/evaluate.py --engine-type daemon-novad   # 對照組: 關掉 VAD
.venv/bin/python eval/evaluate.py --engine-type daemon-itn     # 產品的實際輸出
.venv/bin/python eval/evaluate.py --engine-type sensevoice     # 上游 CLI (R1 用的)
```

`--engine-type` 決定量的是誰。`daemon*` 走 daemon 的 `--transcribe`,
也就是產品實際跑的管線; `sensevoice` 走上游的 `sense-voice-main` CLI。
兩者不是同一條路徑 (CLI 多段切分後 join, daemon 整句解碼), 分數差距
可達 9 個百分點, 方向一致地**低估產品**。見 `docs/ARCHITECTURE.md`。

評測需要 OpenCC 做繁簡摺疊。少了它, 繁體參考文本與簡體辨識結果的每個
字都會被算成錯誤 —— 完全正確的中文會報出 40% 以上的 CER。工具會在
缺少時直接中止而不是靜默給出錯誤數字。

ITN (「三點」→「3点」) 是同一類問題, 目前用 `--no-itn` 迴避而不是
摺疊。產品是開著 ITN 的, 所以 `daemon-itn` 那組才是使用者實際看到的
輸出 —— 但它的分數含表示差異, 不可與其他 engine-type 並列。

---

## 目錄結構

```
voicetype-fcitx5/     fcitx5 module addon (C++, 極薄 —— 只做攔鍵與 commit)
voicetyped/           ASR daemon (Rust, systemd user service)
  src/vad/            Silero VAD: 空錄音防護與首尾修剪 (§4.4)
  src/postproc/       標籤剝除、繁化、詞彙修正 (§4.6 ①②③)
eval/                 語料集與 CER/WER 評測 (SDD §8.1)
config/               預設詞彙表 (安裝到 ~/.config/voicetype/)
systemd/              user service unit
scripts/              安裝、資源量測、煙霧測試
docs/                 架構與設計理由
third_party/          SenseVoice.cpp (MIT)
models/               GGUF 模型 (不進版控)
```

為什麼拆成兩個行程: 見 SDD §3.2。簡短版 —— ASR 崩潰不能拖垮輸入法,
輸入法掛掉等於整個桌面無法打字。

---

## Future work

沒有要做, 但想過的方向:

* **從使用者的修正中學習** —— 聽寫送出後, 使用者常會改幾個字。若能
  觀察到那些修改, 就能自動長出詞彙修正表。三個障礙: `commitString()`
  之後文字屬於應用程式 (要讀 SurroundingText API, 而 Chrome/Electron
  通常不提供)、那等於讀取使用者正在編輯的文字、以及分不出「修正」與
  「改變主意」。比較務實的版本是明確的「修正上一句」動作, 或偵測
  「短時間內重說同一句話」這個訊號 —— 後者 daemon 自己就看得到。
* **更激進的量化** —— q8 相對 fp16 是零損失, 所以 q4 值得一試
  (約 150MB, 省 140MB)。但目前 329MB 已在預算內, 主要價值是更快的
  首次載入而非省 RAM。
* **後處理 ④–⑥** —— 贅字移除、標點正規化, 以及送進 shell 的換行會
  立即執行這個安全問題。

刻意**不**做的: LLM 後處理。日常聽寫要的是快, 規則處理 < 5ms 而 LLM
至少 +300ms, 而且本機 LLM 就是這個專案想避免的 VRAM 佔用來源。

---

## 授權

GPLv3。見 [`LICENSE`](LICENSE)。

本專案程式碼為原創實作。相依的 SenseVoice.cpp 為 MIT, 與 GPLv3 相容。
