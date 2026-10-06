#!/usr/bin/env bash
# Install only the optional GUI. Never replace or restart the speech daemon/addon.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
/usr/bin/python3 - <<'PY'
import gi
gi.require_version('Gtk', '3.0')
gi.require_version('AyatanaAppIndicator3', '0.1')
gi.require_version('Gst', '1.0')
from gi.repository import Gtk, AyatanaAppIndicator3, Gst
PY
VERSION="$(cat "$REPO/scripts/voicetype-settings.py" "$REPO/scripts/voicetype_vocab.py" "$REPO/scripts/voicetype_review.py" "$REPO/scripts/voicetype_review_gui.py" "$REPO/scripts/requirements-settings.txt" | sha256sum | cut -c1-12)"
RUNTIME="$HOME/.local/lib/voicetype/settings-$VERSION"
mkdir -p "$RUNTIME"
if [[ ! -x "$RUNTIME/venv/bin/python" ]]; then
    /usr/bin/python3 -m venv --system-site-packages "$RUNTIME/venv"
fi
"$RUNTIME/venv/bin/python" -m pip install -r "$REPO/scripts/requirements-settings.txt"
install -m 644 "$REPO/scripts/voicetype-settings.py" "$RUNTIME/voicetype-settings.py"
install -m 644 "$REPO/scripts/voicetype_vocab.py" "$RUNTIME/voicetype_vocab.py"
install -m 644 "$REPO/scripts/voicetype_review.py" "$RUNTIME/voicetype_review.py"
install -m 644 "$REPO/scripts/voicetype_review_gui.py" "$RUNTIME/voicetype_review_gui.py"
"$RUNTIME/venv/bin/python" - "$RUNTIME" "$REPO" <<'PY'
import importlib.util, os, shlex, sys
from pathlib import Path
runtime, repo = map(Path, sys.argv[1:])
sys.path.insert(0, str(runtime))
spec = importlib.util.spec_from_file_location('settings', runtime / 'voicetype-settings.py')
settings = importlib.util.module_from_spec(spec)
spec.loader.exec_module(settings)
from voicetype_vocab import atomic_write
launcher = Path.home() / '.local/bin/voicetype-settings'
script = ('#!/bin/sh\nexport VOICETYPE_SETTINGS_LAUNCHER=' + shlex.quote(str(launcher)) + '\nexec '
          + shlex.quote(str(runtime / 'venv/bin/python')) + ' '
          + shlex.quote(str(runtime / 'voicetype-settings.py')) + ' "$@"\n')
atomic_write(launcher, script.encode(), 0o755)
data = Path(os.environ.get('XDG_DATA_HOME', Path.home() / '.local/share'))
atomic_write(data / 'icons/hicolor/scalable/apps/voicetype-settings.svg',
             (repo / 'assets/voicetype-settings.svg').read_bytes(), 0o644)
desktop = data / ('applications/' + settings.APP_ID + '.desktop')
atomic_write(desktop, settings.desktop_entry(launcher).encode(), 0o644)
# Preserve an existing choice on upgrades, including a removed autostart file.
marker = Path(os.environ.get('XDG_CONFIG_HOME', Path.home() / '.config')) / 'voicetype/settings-installed'
if not marker.exists():
    if not settings.autostart_path().exists():
        atomic_write(settings.autostart_path(), settings.desktop_entry(launcher, tray=True).encode())
    atomic_write(marker, b'installed\n')
print('Installed:', launcher)
print('Desktop entry:', desktop)
print('Runtime:', runtime)
PY
if command -v update-desktop-database >/dev/null; then
    update-desktop-database "${XDG_DATA_HOME:-$HOME/.local/share}/applications"
fi
if command -v gtk-update-icon-cache >/dev/null; then
    gtk-update-icon-cache -f -t "${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
fi
printf 'Open: %s/.local/bin/voicetype-settings\n' "$HOME"
