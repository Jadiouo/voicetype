#!/usr/bin/env bash
# Install an already-built Fcitx addon for this user, without sudo.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD="${1:-$REPO/voicetype-fcitx5/build}"
LIB_DIR="$HOME/.local/lib/fcitx5"
CONF_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/fcitx5/addon"
[[ -f "$BUILD/src/libvoicetype.so" && -f "$BUILD/src/voicetype.conf" ]] || {
    printf 'Build addon first; missing files under %s/src\n' "$BUILD" >&2; exit 1;
}
mkdir -p "$LIB_DIR" "$CONF_DIR"
# Rename atomically: the existing module may still be mapped by Fcitx.
install -m 755 "$BUILD/src/libvoicetype.so" "$LIB_DIR/libvoicetype.so.new"
mv "$LIB_DIR/libvoicetype.so.new" "$LIB_DIR/libvoicetype.so"
python3 - "$BUILD/src/voicetype.conf" "$CONF_DIR/voicetype.conf" "$LIB_DIR/libvoicetype" <<'PY'
import pathlib, re, sys
source, destination, library = sys.argv[1:]
data = pathlib.Path(source).read_text()
data, count = re.subn(r'^Library=.*$', lambda _: 'Library=' + library, data, flags=re.M)
if count != 1:
    raise SystemExit('Addon config must have exactly one Library entry')
target = pathlib.Path(destination)
temporary = target.with_suffix('.conf.new')
temporary.write_text(data)
temporary.replace(target)
PY
printf 'Installed user addon. Restart Fcitx when ready: fcitx5 -rd\n'
