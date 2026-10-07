"""Record only built preview artifacts; never collect user settings or transcripts."""
import hashlib
import json
import os
from pathlib import Path

root = Path("target/release/bundle")
files = sorted(root.glob("deb/*.deb")) + sorted(root.glob("nsis/*-setup.exe"))
if not files:
    raise SystemExit("No installer was produced")
report = {
    "kind": "desktop-preview-linux-local-input",
    "source_commit": os.environ.get("GITHUB_SHA", "local-unpublished"),
    "runner_os": os.environ.get("RUNNER_OS", os.name),
    "linux_cpu_runtime_bundled": Path("target/runtime/manifest.json").is_file(),
    "fcitx_input_bundled": Path("target/input/manifest.json").is_file(),
    "end_to_end_dictation_verified": False,
    "artifacts": [
        {"name": p.name, "bytes": p.stat().st_size, "sha256": hashlib.sha256(p.read_bytes()).hexdigest()}
        for p in files
    ],
}
Path("target/preview-manifest.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
print(json.dumps(report, indent=2))
