#!/usr/bin/env python3
"""Build and catalog narrow Google CLI helpers; no official CLI or account is bundled."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[1]
    output = args.output.absolute()
    if output.exists() or output.is_symlink():
        raise SystemExit("Destination exists; choose a new bundle directory")
    target = Path(os.environ.get("CARGO_TARGET_DIR", repo / "desktop/target")).absolute()
    subprocess.run(["cargo", "build", "--manifest-path", str(repo / "desktop/Cargo.toml"),
                    "-p", "voicetype-app-core", "--bin", "voicetype-google-editor",
                    "--bin", "voicetype-google-pcm", "--locked", "--release", "-j", "2"],
                   check=True)
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".google-stage-", dir=output.parent) as stage_name:
        stage = Path(stage_name)
        suffix = ".exe" if os.name == "nt" else ""
        files = []
        for name in ("voicetype-google-editor", "voicetype-google-pcm"):
            source = target / "release" / (name + suffix)
            destination = stage / (name + suffix)
            shutil.copyfile(source, destination)
            if os.name != "nt":
                destination.chmod(0o755)
            data = destination.read_bytes()
            files.append({"path": destination.name, "bytes": len(data),
                          "sha256": hashlib.sha256(data).hexdigest(), "executable": True})
        shutil.copyfile(repo / "LICENSE", stage / "LICENSE.txt")
        data = (stage / "LICENSE.txt").read_bytes()
        files.append({"path": "LICENSE.txt", "bytes": len(data),
                      "sha256": hashlib.sha256(data).hexdigest(), "executable": False})
        catalog = {"schema_version": 1, "id": "google-helpers",
                   "version": "source-" + files[0]["sha256"][:16],
                   "platform": "windows-x86_64" if os.name == "nt" else "linux-x86_64",
                   "source_url": "https://github.com/Jadiouo/voicetype",
                   "license": "GPL-3.0-only", "files": files}
        (stage / "manifest.json").write_text(json.dumps(catalog, indent=2) + "\n")
        if output.exists() or output.is_symlink():
            raise SystemExit("Destination appeared during the build; preserved it")
        stage.rename(output)
    print(f"Prepared only: {output}")


if __name__ == "__main__":
    main()
