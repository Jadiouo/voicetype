#!/usr/bin/env bash
#
# 安裝 VoiceType 為 systemd user service。
#
# 裝 daemon 執行檔、放好模型與詞彙表、註冊並啟動 service, 最後提示
# addon 的安裝 (那步需要 sudo, 刻意不代勞)。
#
# 每一步失敗都直接中止並說明原因。這個專案被「回報成功但沒有效果」
# 咬過五次 (見 docs/devlog.md), 安裝腳本是最容易再犯的地方。

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_SRC="$REPO/voicetyped/target/release/voicetyped"
BIN_DST="$HOME/.local/bin/voicetyped"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/voicetype"
MODEL_NAME="sense-voice-small-q8_0.gguf"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"

say() { printf '\n\033[1m%s\033[0m\n' "$*"; }
die() { printf '\n\033[31m錯誤: %s\033[0m\n' "$*" >&2; exit 1; }

# --- 1. daemon 執行檔 -------------------------------------------------

[[ -x "$BIN_SRC" ]] || die "找不到 daemon 執行檔: $BIN_SRC

先建置:
  cd $REPO/voicetyped && cargo build --release --features sensevoice"

# 不含引擎的建置會在啟動時拒絕跑, 但那要等到 service 啟動失敗才發現。
# 這裡先問清楚 —— 兩種 feature 配置產出同一個檔名, 覆蓋是常見意外。
#
# 用 `grep -c` 而不是 `grep -q`: 後者命中就立刻退出, strings 收到
# SIGPIPE, 而 `set -o pipefail` 會把整條管道判成失敗 —— 檢查因此
# 永遠說「沒有引擎」, 包括執行檔完全正常的時候。
if [[ "$(strings "$BIN_SRC" | grep -c sense_voice_small_init || true)" -eq 0 ]]; then
    die "這個執行檔不含 ASR 引擎 (辨識結果會是 [null asr: ...])。

兩種 feature 配置產出同一個 target/release/voicetyped, 跑過一次
不帶 feature 的 cargo build 就會覆蓋掉。重新建置:
  cd $REPO/voicetyped && cargo build --release --features sensevoice"
fi

say "安裝 daemon → $BIN_DST"
mkdir -p "$(dirname "$BIN_DST")"
install -m 755 "$BIN_SRC" "$BIN_DST"

case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) printf '  注意: %s 不在 PATH 裡。service 用絕對路徑, 不受影響。\n' "$HOME/.local/bin" ;;
esac

# --- 2. 模型 ----------------------------------------------------------

mkdir -p "$DATA_DIR/models"
MODEL_DST="$DATA_DIR/models/$MODEL_NAME"

if [[ -f "$MODEL_DST" ]]; then
    say "模型已就位 → $MODEL_DST"
elif [[ -f "$REPO/models/$MODEL_NAME" ]]; then
    say "複製模型 → $MODEL_DST (291MB, 需要一點時間)"
    # 複製而不是 symlink: service 有 ProtectHome=read-only, 而模型
    # 路徑要能在 repo 被移動或刪除後仍然有效。
    cp "$REPO/models/$MODEL_NAME" "$MODEL_DST"
else
    die "找不到模型。下載:

  mkdir -p '$DATA_DIR/models'
  curl -L -o '$MODEL_DST' \\
    https://huggingface.co/lovemefan/sense-voice-gguf/resolve/main/$MODEL_NAME"
fi

# --- 3. 詞彙修正表 (SDD §4.6 ③) --------------------------------------

CONF_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/voicetype"
VOCAB="$CONF_DIR/vocab.toml"
mkdir -p "$CONF_DIR"

if [[ -f "$VOCAB" ]]; then
    # 這是使用者會編輯的檔案, 覆蓋等於刪掉他們加的詞。
    say "詞彙表已存在, 保留不動 → $VOCAB"
else
    say "安裝預設詞彙表 → $VOCAB"
    install -m 644 "$REPO/config/vocab.toml" "$VOCAB"
fi

# --- 4. systemd user service -----------------------------------------

# 手動跑的 daemon 會佔著 socket, service 起來後拒絕搶佔並退出
# (那個拒絕是對的 —— 兩個 daemon 搶同一個 socket, addon 連到誰是
# 未定義的)。先停掉 service 自己的實例, 剩下的就是手動跑的。
systemctl --user stop voicetyped.service 2>/dev/null || true
SOCK="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/voicetype/ipc.sock"
if [[ -S "$SOCK" ]] && pgrep -u "$USER" -x voicetyped >/dev/null 2>&1; then
    die "有一個手動啟動的 voicetyped 正在跑, 佔著 $SOCK

先停掉它 (跑它的那個終端按 Ctrl-C), 或:
  pkill -u $USER -x voicetyped

然後重跑這個腳本。"
fi

say "註冊 service → $UNIT_DIR/voicetyped.service"
mkdir -p "$UNIT_DIR"
install -m 644 "$REPO/systemd/voicetyped.service" "$UNIT_DIR/voicetyped.service"

systemctl --user daemon-reload
systemctl --user enable --now voicetyped.service

# enable --now 回傳成功不代表 daemon 活著 —— Type=simple 下 systemd
# 只確認 fork 成功。等一下再看實際狀態。
sleep 2

# 檢查 SubState 而不是只看 is-active: 一個不斷崩潰重啟的 service 會在
# `activating (auto-restart)` 與 `failed` 之間擺盪, 而 `is-active` 在
# auto-restart 那一瞬間回傳成功 —— 實測就這樣放行過一次啟動失敗的
# daemon。要 `active running` 才算數。
STATE="$(systemctl --user show voicetyped.service -p ActiveState -p SubState --value | tr '\n' ' ')"
if [[ "$STATE" != "active running "* ]]; then
    printf '\n\033[31mservice 啟動失敗:\033[0m\n\n'
    systemctl --user status voicetyped.service --no-pager --lines=20 || true
    exit 1
fi

say "daemon 已啟動"
systemctl --user status voicetyped.service --no-pager --lines=3 | sed 's/^/  /'

# --- 5. addon (需要 sudo, 不代勞) -------------------------------------

ADDON_SO="/usr/lib/x86_64-linux-gnu/fcitx5/libvoicetype.so"
ADDON_CONF="/usr/share/fcitx5/addon/voicetype.conf"

if [[ -f "$ADDON_SO" && -f "$ADDON_CONF" ]]; then
    say "fcitx5 addon 已安裝"
else
    say "還差 fcitx5 addon —— 這步需要 sudo, 請自己跑:"
    cat <<EOF

  cd $REPO/voicetype-fcitx5
  cmake -B build -DCMAKE_BUILD_TYPE=RelWithDebInfo -DCMAKE_INSTALL_PREFIX=/usr
  cmake --build build
  sudo cmake --install build
  fcitx5 -rd
EOF
fi

cat <<'EOF'

完成。按住 Ctrl+Alt 說話, 放開即送出; 錄音中按 Esc 取消。

  systemctl --user status voicetyped     # 狀態
  journalctl --user -u voicetyped -f     # 即時 log
  systemctl --user restart voicetyped    # 換了新版本之後

EOF
