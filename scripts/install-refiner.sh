#!/usr/bin/env bash
# Install an already-built llama.cpp runtime and enable CPU-only refinement.
# Obtain/build the pinned runtime and model as documented in docs/PERSONALIZATION.md.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE="${1:?Usage: install-refiner.sh /path/to/llama.cpp/build/bin}"
DATA="$HOME/.local/share/voicetype"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
[[ -x "$SOURCE/llama-server" ]] || { printf 'Missing llama-server\n' >&2; exit 1; }
[[ -s "$DATA/models/Qwen3-0.6B-Q8_0.gguf" ]] || { printf 'Missing Qwen3 model; see docs/PERSONALIZATION.md\n' >&2; exit 1; }
# The versioned directory avoids overwriting any running shared libraries.
RUNTIME="$DATA/llama-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$RUNTIME" "$UNIT_DIR/voicetyped.service.d"
install -m 755 "$SOURCE/llama-server" "$RUNTIME/llama-server"
shopt -s nullglob
libraries=("$SOURCE"/*.so*)
if (( ${#libraries[@]} )); then cp -a "${libraries[@]}" "$RUNTIME/"; fi
[[ ! -e "$DATA/llama" || -L "$DATA/llama" ]] || {
    printf '%s exists as a directory; retain it and choose a new installation path.\n' "$DATA/llama" >&2; exit 1;
}
ln -sfn "$(basename "$RUNTIME")" "$DATA/llama.new"
mv -Tf "$DATA/llama.new" "$DATA/llama"
install -m 644 "$REPO/systemd/voicetype-refiner.service" "$UNIT_DIR/voicetype-refiner.service"
cat > "$UNIT_DIR/voicetyped.service.d/refiner.conf" <<'EOF'
[Service]
Environment=VOICETYPE_REFINER_URL=http://127.0.0.1:18765
Environment=VOICETYPE_REFINER_MODE=faithful
Environment=VOICETYPE_REFINER_TIMEOUT_MS=2500
EOF
systemctl --user daemon-reload
systemctl --user enable --now voicetype-refiner.service
systemctl --user restart voicetype-refiner.service
python3 - <<'PY'
import time, urllib.request
for _ in range(50):
    try:
        with urllib.request.urlopen('http://127.0.0.1:18765/health', timeout=1) as r:
            if r.status == 200:
                break
    except OSError:
        time.sleep(0.2)
else:
    raise SystemExit('Refiner did not become healthy; inspect journalctl --user -u voicetype-refiner')
PY
STATE="$(systemctl --user show voicetype-refiner.service -p ActiveState -p SubState --value | tr '\n' ' ')"
[[ "$STATE" == "active running "* ]] || { printf 'Refiner service is not running\n' >&2; exit 1; }
systemctl --user restart voicetyped.service
printf 'CPU refiner enabled. Verify output with voicetype-control process and eval/evaluate_context.py.\n'
