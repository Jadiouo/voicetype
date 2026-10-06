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
# Validate explicit-selection dependencies before replacing any installed file.
command -v xprop >/dev/null || { printf 'Missing xprop: install x11-utils\n' >&2; exit 1; }
/usr/bin/python3 -c 'import gi; gi.require_version("Atspi", "2.0"); from gi.repository import Atspi' || {
    printf 'Install python3-gi and gir1.2-atspi-2.0 for explicit selection learning\n' >&2; exit 1;
}
BIN_DIR="$HOME/.local/bin"
mkdir -p "$LIB_DIR" "$CONF_DIR" "$BIN_DIR"
install -m 755 "$REPO/scripts/context-selection.py" "$BIN_DIR/voicetype-selection.new"
mv "$BIN_DIR/voicetype-selection.new" "$BIN_DIR/voicetype-selection"
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
