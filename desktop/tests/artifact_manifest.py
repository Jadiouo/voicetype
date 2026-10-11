"""Record only built preview artifacts; never collect user settings or transcripts."""
import hashlib
import json
import os
from pathlib import Path

root = Path("target/release/bundle")
spelling_origins = Path("target/spelling/provenance/binary-origins.json")
rust_notices = Path("target/rust-notices/manifest.json")
installer_pe = Path("target/installer-pe-audit.json")
installer_status = json.loads(installer_pe.read_text()) if os.name == "nt" and installer_pe.is_file() else {}
distribution_pe = Path("target/windows-pe-provenance.json")
distribution_status = json.loads(distribution_pe.read_text()) if os.name == "nt" and distribution_pe.is_file() else {}
files = sorted(root.glob("deb/*.deb")) + sorted(root.glob("nsis/*-setup.exe"))
if not files:
    raise SystemExit("No installer was produced")
report = {
    "kind": "desktop-preview-platform-local-input",
    "source_commit": os.environ.get("GITHUB_SHA", "local-unpublished"),
    "runner_os": os.environ.get("RUNNER_OS", os.name),
    "cpu_runtime_bundled": Path("target/runtime/manifest.json").is_file(),
    "linux_cpu_runtime_bundled": os.name != "nt" and Path("target/runtime/manifest.json").is_file(),
    "windows_cpu_runtime_bundled": os.name == "nt" and Path("target/runtime/manifest.json").is_file(),
    "cpu_spelling_bundled": Path("target/spelling/manifest.json").is_file(),
    "cpu_spelling_native_notices_complete": json.loads(spelling_origins.read_text())["complete"] if spelling_origins.is_file() else False,
    "rust_target_notices_complete": json.loads(rust_notices.read_text())["complete"] if rust_notices.is_file() else False,
    "windows_installer_plugins_verified": bool(installer_status.get("plugins_verified") and
                                                installer_status.get("installed_notices_verified")),
    "windows_pe_provenance_complete": distribution_status.get("complete") is True,
    "msvc_redistribution_eligibility": distribution_status.get("msvc_redistribution_eligibility", "not_assessed"),
    "fcitx_input_bundled": Path("target/input/manifest.json").is_file(),
    "end_to_end_dictation_verified": False,
    "artifacts": [
        {"name": p.name, "bytes": p.stat().st_size, "sha256": hashlib.sha256(p.read_bytes()).hexdigest()}
        for p in files
    ],
}
Path("target/preview-manifest.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
print(json.dumps(report, indent=2))
