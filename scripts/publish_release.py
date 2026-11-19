#!/usr/bin/env python3
"""Publish or resume an already validated zterm candidate."""

import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time
import tarfile
import tomllib
from urllib.error import HTTPError
from urllib.request import Request, urlopen

REPOSITORY = "LIghtJUNction/zterm"
USER_AGENT = "zterm-release (+https://github.com/LIghtJUNction/zterm)"


def run(*command: str) -> None:
    subprocess.run(command, check=True)


def output(*command: str) -> str:
    return subprocess.check_output(command, text=True).strip()


def require_environment(name: str) -> str:
    value = os.environ.get(name, "")
    if not value:
        raise ValueError(f"{name} is required for publication")
    return value


def read_url(url: str, *, github: bool = False) -> bytes | None:
    headers = {"User-Agent": USER_AGENT, "Cache-Control": "no-cache"}
    if github:
        if not url.startswith("https://api.github.com/"):
            raise ValueError("GitHub credentials require the GitHub API origin")
        headers["Authorization"] = "Bearer " + require_environment("GH_TOKEN")
        headers["Accept"] = "application/vnd.github+json"
        headers["X-GitHub-Api-Version"] = "2026-03-10"
    try:
        with urlopen(Request(url, headers=headers), timeout=30) as response:
            return response.read()
    except HTTPError as error:
        if error.code == 404:
            return None
        raise


def registry_checksum(version: str) -> str | None:
    data = read_url("https://index.crates.io/zt/er/zterm")
    if data is None:
        return None
    entries = (json.loads(line) for line in data.splitlines() if line)
    matches = [entry for entry in entries if entry.get("vers") == version]
    if len(matches) > 1:
        raise ValueError("Duplicate registry versions")
    if not matches:
        return None
    entry = matches[0]
    if entry.get("yanked"):
        raise ValueError("The existing crate version is yanked")
    checksum = entry["cksum"]
    if not re.fullmatch(r"[0-9a-f]{64}", checksum):
        raise ValueError("Invalid registry checksum")
    return checksum


def require_matching_checksum(actual: str, expected: str, label: str) -> None:
    if actual != expected:
        raise ValueError(f"{label} differs from the validated candidate")


def release_record(tag: str) -> dict | None:
    data = read_url(
        f"https://api.github.com/repos/{REPOSITORY}/releases/tags/{tag}",
        github=True,
    )
    return json.loads(data) if data is not None else None


def verify_asset(asset: dict, crate: Path, checksum: str) -> None:
    if asset.get("state") != "uploaded" or asset.get("size") != crate.stat().st_size:
        raise ValueError("The GitHub crate asset has an unexpected state or size")
    digest = asset.get("digest")
    if digest is not None:
        require_matching_checksum(digest, "sha256:" + checksum, "GitHub crate asset")
        return
    data = read_url(asset["browser_download_url"])
    if data is None:
        raise ValueError("The existing GitHub crate asset is unavailable")
    require_matching_checksum(hashlib.sha256(data).hexdigest(), checksum, "GitHub crate asset")


def finish_github_release(tag: str, version: str, crate: Path, checksum: str) -> None:
    record = release_record(tag)
    if record is None:
        run(
            "gh", "release", "create", tag, str(crate) + "#Rust source crate",
            "--repo", REPOSITORY, "--verify-tag", "--title", "zterm " + version,
            "--generate-notes",
        )
    else:
        assets = [asset for asset in record["assets"] if asset["name"] == crate.name]
        if len(assets) > 1:
            raise ValueError("Duplicate GitHub crate assets")
        if assets:
            verify_asset(assets[0], crate, checksum)
        else:
            run("gh", "release", "upload", tag, str(crate), "--repo", REPOSITORY)
        if record["draft"]:
            run("gh", "release", "edit", tag, "--draft=false", "--repo", REPOSITORY)
    record = release_record(tag)
    if record is None or record["draft"] or record["tag_name"] != tag:
        raise ValueError("The published GitHub Release did not read back")
    assets = [asset for asset in record["assets"] if asset["name"] == crate.name]
    if len(assets) != 1:
        raise ValueError("The published GitHub crate asset did not read back")
    verify_asset(assets[0], crate, checksum)
    print(record["html_url"])


def main() -> None:
    require_environment("CARGO_REGISTRY_TOKEN")
    require_environment("GH_TOKEN")
    version = require_environment("VERSION")
    candidate = require_environment("CANDIDATE_SHA")
    if not re.fullmatch(r"[0-9a-f]{40}", candidate):
        raise ValueError("CANDIDATE_SHA must be a full commit SHA")
    if require_environment("GITHUB_REPOSITORY").lower() != REPOSITORY.lower():
        raise ValueError("Publish from the zterm repository")
    if output("git", "rev-parse", "HEAD") != candidate:
        raise ValueError("Checkout does not match the validated candidate")
    if output("git", "status", "--porcelain"):
        raise ValueError("Publication requires a clean candidate checkout")
    run("git", "fetch", "--tags", "origin", "refs/heads/main:refs/remotes/origin/main")
    run("git", "merge-base", "--is-ancestor", candidate, "origin/main")
    package = tomllib.loads(Path("Cargo.toml").read_text(encoding="utf-8"))["package"]
    if package["name"] != "zterm" or package["version"] != version:
        raise ValueError("Package identity differs from the candidate outputs")
    run("python3", "tools/check_package.py")
    run("cargo", "package", "--locked")
    crate = Path("target/package") / f"zterm-{version}.crate"
    with tarfile.open(crate) as archive:
        member = archive.extractfile(f"zterm-{version}/.cargo_vcs_info.json")
        if member is None:
            raise ValueError("Packaged crate is missing its source identity")
        vcs = json.load(member)
    if vcs["git"]["sha1"] != candidate or vcs["git"].get("dirty", False):
        raise ValueError("Packaged source differs from the clean candidate")
    checksum = hashlib.sha256(crate.read_bytes()).hexdigest()
    existing = registry_checksum(version)
    if existing is not None:
        require_matching_checksum(existing, checksum, "Registry crate")

    tag = "v" + version
    found = subprocess.run(
        ["git", "show-ref", "--verify", "--quiet", "refs/tags/" + tag], check=False
    )
    if found.returncode == 0:
        if output("git", "rev-parse", f"refs/tags/{tag}^{{}}") != candidate:
            raise ValueError("Existing release tag points to another commit")
    elif found.returncode != 1:
        raise ValueError("Could not inspect the existing release tag")

    if existing is None:
        published = subprocess.run(["cargo", "publish", "--locked"], check=False)
        for attempt in range(12):
            existing = registry_checksum(version)
            if existing is not None:
                require_matching_checksum(existing, checksum, "Registry crate")
                break
            if attempt != 11:
                time.sleep(5)
        if existing is None:
            raise RuntimeError(
                f"Registry confirmation is pending (cargo exit {published.returncode}); "
                f"resume candidate {candidate}"
            )
    if found.returncode == 1:
        run("git", "config", "user.name", "github-actions[bot]")
        run(
            "git", "config", "user.email",
            "41898282+github-actions[bot]@users.noreply.github.com",
        )
        run("git", "tag", "-a", tag, candidate, "-m", "zterm " + version)
    run("git", "push", "origin", "refs/tags/" + tag)
    print(f"Verified crates.io zterm {version} from {candidate}")
    finish_github_release(tag, version, crate, checksum)


if __name__ == "__main__":
    main()
