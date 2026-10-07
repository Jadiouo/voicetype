"""Discard generated installer/evidence outputs restored by the Rust CI cache.

Keep Rust compilation caches. Never touch config/model stores or daily installs.
Run after cache restore and before any build/test report is created.
"""
from pathlib import Path
import shutil

target = Path(__file__).resolve().parents[1] / "target"
for relative in ("runtime", "inspection", "shell-evidence", "release/bundle", "preview-manifest.json"):
    path = target / relative
    if path.is_symlink():
        path.unlink()
    elif path.is_dir():
        shutil.rmtree(path)
    elif path.is_file():
        path.unlink()
