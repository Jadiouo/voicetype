"""Fetch two pinned, real Windows preview installers for the upgrade smoke.

The old package is a reviewed CI artifact. The new run, commit and setup digest
must be supplied independently when manually dispatching the upgrade job.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import zipfile


REPOSITORY = "Jadiouo/voicetype"
ARTIFACT_NAME = "voicetype-preview-windows-2022"
WINDOWS_JOB = "preview (windows-2022, nsis)"
WORKFLOW = ".github/workflows/desktop-preview.yml"
OLD_RUN = 37808930921
OLD_HEAD = "7e1944f0c9a6aa45aa48c4f1a77ab4b668c8e8f4"
OLD_ARTIFACT = 11565925281
OLD_SHA256 = "6c1b78f27df2ea8cacd44ade67c8d0c02fa69cc0d0ad8c54a005794b45c52e41"
OLD_BYTES = 182315207
OLD_VERSION = "0.2.0-alpha.1"
NEW_VERSION = "0.2.1-alpha.1"


def api_json(route):
    result = subprocess.run(["gh", "api", f"repos/{REPOSITORY}/{route}"],
                            capture_output=True, text=True, check=True, timeout=30)
    return json.loads(result.stdout)


def artifact_id(run_id, head, expected_id=None):
    run = api_json(f"actions/runs/{run_id}")
    if (run.get("id") != run_id or run.get("head_sha") != head
            or run.get("head_branch") != "feat/desktop-app"
            or run.get("head_repository", {}).get("full_name") != REPOSITORY
            or run.get("path") != WORKFLOW or run.get("status") != "completed"
            or run.get("event") not in ("push", "workflow_dispatch")):
        raise RuntimeError(f"CI run {run_id} has the wrong source or is unfinished")
    jobs = api_json(f"actions/runs/{run_id}/jobs?per_page=100").get("jobs", [])
    windows = [job for job in jobs if job.get("name") == WINDOWS_JOB]
    if (len(windows) != 1 or windows[0].get("head_sha") != head
            or windows[0].get("status") != "completed"
            or windows[0].get("conclusion") != "success"):
        raise RuntimeError(f"CI run {run_id} has no single successful Windows NSIS job")
    artifacts = api_json(f"actions/runs/{run_id}/artifacts?per_page=100").get("artifacts", [])
    matching = [item for item in artifacts if item.get("name") == ARTIFACT_NAME]
    if (len(matching) != 1 or matching[0].get("expired") is not False
            or matching[0].get("workflow_run", {}).get("id") != run_id
            or matching[0].get("workflow_run", {}).get("head_sha") != head
            or (expected_id is not None and matching[0].get("id") != expected_id)):
        raise RuntimeError(f"CI run {run_id} has no single matching live artifact")
    return matching[0]["id"]


def safe_members(archive):
    seen = set()
    names = set()
    for member in archive.infolist():
        name = member.filename
        path = PurePosixPath(name)
        mode = (member.external_attr >> 16) & 0o170000
        if (not name or "\\" in name or path.is_absolute()
                or any(part in ("", ".", "..") for part in path.parts)
                or mode == stat.S_IFLNK or path.as_posix().casefold() in seen):
            raise RuntimeError(f"unsafe or duplicate CI artifact member: {name!r}")
        seen.add(path.as_posix().casefold())
        names.add(path.as_posix())
    return names


def fetch_package(artifact, root, version, head, expected_sha, expected_bytes=None):
    root.mkdir(parents=True, exist_ok=False)
    archive_path = root / "artifact.zip"
    with archive_path.open("wb") as output:
        subprocess.run(["gh", "api", f"repos/{REPOSITORY}/actions/artifacts/{artifact}/zip"],
                       stdout=output, check=True, timeout=600)
    name = f"VoiceType Preview_{version}_x64-setup.exe"
    setup_member = f"release/bundle/nsis/{name}"
    with zipfile.ZipFile(archive_path) as archive:
        members = safe_members(archive)
        setups = [item for item in members if item.endswith("-setup.exe")]
        if setups != [setup_member] or "preview-manifest.json" not in members:
            raise RuntimeError("CI artifact does not contain exactly the expected setup and manifest")
        manifest = json.loads(archive.read("preview-manifest.json"))
        if (manifest.get("source_commit") != head or manifest.get("runner_os") != "Windows"
                or manifest.get("kind") not in ("desktop-preview-linux-local-input",
                                                "desktop-preview-platform-local-input")
                or len(manifest.get("artifacts", [])) != 1):
            raise RuntimeError("CI manifest source or platform differs from run")
        entry = manifest["artifacts"][0]
        if (entry.get("name") != name or entry.get("sha256") != expected_sha
                or not isinstance(entry.get("bytes"), int) or isinstance(entry["bytes"], bool)
                or entry["bytes"] <= 0
                or (expected_bytes is not None and entry["bytes"] != expected_bytes)):
            raise RuntimeError("CI manifest setup identity differs from independent pin")
        output = root / setup_member
        output.parent.mkdir(parents=True)
        digest = hashlib.sha256()
        size = 0
        with archive.open(setup_member) as source, output.open("wb") as target:
            while chunk := source.read(1024 * 1024):
                target.write(chunk)
                digest.update(chunk)
                size += len(chunk)
        if size != entry["bytes"] or digest.hexdigest() != expected_sha:
            raise RuntimeError("downloaded setup bytes differ from manifest and independent pin")
        (root / "preview-manifest.json").write_bytes(archive.read("preview-manifest.json"))
    archive_path.unlink()
    return {"artifact_id": artifact, "source_commit": head, "setup": str(output),
            "bytes": size, "sha256": expected_sha, "version": version}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--new-run-id", required=True)
    parser.add_argument("--new-head-sha", required=True)
    parser.add_argument("--new-setup-sha256", required=True)
    args = parser.parse_args()
    if (os.name != "nt" or os.environ.get("RUNNER_OS") != "Windows"
            or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted"
            or os.environ.get("GITHUB_ACTIONS") != "true"
            or os.environ.get("GITHUB_REPOSITORY") != REPOSITORY):
        raise RuntimeError("upgrade input download requires the repository's hosted Windows runner")
    if (not re.fullmatch(r"[1-9][0-9]*", args.new_run_id)
            or not re.fullmatch(r"[0-9a-f]{40}", args.new_head_sha)
            or not re.fullmatch(r"[0-9a-f]{64}", args.new_setup_sha256)
            or args.new_head_sha == OLD_HEAD or args.new_setup_sha256 == OLD_SHA256):
        raise RuntimeError("new run, full commit SHA and independently pinned setup SHA are required")
    workspace = Path(os.environ["GITHUB_WORKSPACE"]).resolve(strict=True)
    root = workspace / ".scratch/windows-upgrade-inputs"
    root.mkdir(parents=True, exist_ok=False)
    old_id = artifact_id(OLD_RUN, OLD_HEAD, OLD_ARTIFACT)
    new_id = artifact_id(int(args.new_run_id), args.new_head_sha)
    old = fetch_package(old_id, root / "old", OLD_VERSION, OLD_HEAD, OLD_SHA256, OLD_BYTES)
    new = fetch_package(new_id, root / "new", NEW_VERSION, args.new_head_sha,
                        args.new_setup_sha256)
    (root / "input-report.json").write_text(json.dumps({"old": old, "new": new}, indent=2) + "\n")
    print(json.dumps({"old": old, "new": new}, indent=2))


if __name__ == "__main__":
    main()
