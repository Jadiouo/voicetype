#!/usr/bin/env python3
"""Build and catalog the Linux Fcitx module; no installation or service restart."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import shlex
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
    with tempfile.TemporaryDirectory(prefix="voicetype-input-build-") as temporary:
        build = Path(temporary) / "build"
        subprocess.run(["cmake", "-S", str(repo / "voicetype-fcitx5"), "-B", str(build),
                        "-DFCITX_INSTALL_USE_FCITX_SYS_PATHS=ON", "-DBUILD_TESTING=OFF",
                        "-DCMAKE_BUILD_TYPE=Release", "-DVOICETYPE_REQUIRE_X11_CAPS_RESTORE=ON",
                        "-DCMAKE_CXX_FLAGS=" + shlex.quote(f"-ffile-prefix-map={repo}=/voicetype")], check=True)
        subprocess.run(["cmake", "--build", str(build), "--target", "voicetype", "-j", "2"], check=True)
        output.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix=".input-stage-", dir=output.parent) as stage:
            stage = Path(stage)
            shutil.copyfile(build / "src/libvoicetype.so", stage / "libvoicetype.so")
            shutil.copyfile(repo / "LICENSE", stage / "LICENSE.txt")
            files = []
            for path in sorted(stage.iterdir()):
                data = path.read_bytes()
                files.append({"path": path.name, "bytes": len(data),
                              "sha256": hashlib.sha256(data).hexdigest(), "executable": False})
            catalog = {"schema_version": 1, "id": "fcitx-input",
                       "version": "source-" + files[1]["sha256"][:16], "platform": "linux-x86_64",
                       "source_url": "https://github.com/Jadiouo/voicetype",
                       "license": "GPL-3.0-only", "files": files}
            (stage / "manifest.json").write_text(json.dumps(catalog, indent=2) + "\n")
            # mv -T -n prevents another builder replacing a completed output.
            subprocess.run(["mv", "-T", "--no-clobber", "--", str(stage), str(output)], check=True)
            if stage.exists():
                raise SystemExit("Destination appeared during the build; preserved it")
    print(f"Prepared only: {output}")

if __name__ == "__main__":
    main()
