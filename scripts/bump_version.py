#!/usr/bin/env python3
"""Prepare the package version and lockfile before candidate validation."""

import argparse
import os
from pathlib import Path
import re
import subprocess
import tomllib


def next_version(current: str, bump: str) -> str:
    if bump not in ("none", "patch", "minor", "major"):
        raise ValueError("Expected none, patch, minor or major")
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", current):
        raise ValueError("Release bumps require a stable major.minor.patch version")
    if bump == "none":
        return current
    major, minor, patch = map(int, current.split("."))
    if bump == "major":
        return f"{major + 1}.0.0"
    if bump == "minor":
        return f"{major}.{minor + 1}.0"
    return f"{major}.{minor}.{patch + 1}"


def replace_package_version(text: str, version: str) -> str:
    header = re.search(r"(?m)^\[package\][ \t]*$", text)
    if header is None:
        raise ValueError("Missing [package] section")
    start = header.end()
    following = re.search(r"(?m)^\[", text[start:])
    end = start + following.start() if following else len(text)
    section, count = re.subn(
        r'(?m)^(version[ \t]*=[ \t]*")[^"]+("[ \t]*(?:#.*)?)$',
        lambda match: match.group(1) + version + match.group(2),
        text[start:end],
    )
    if count != 1:
        raise ValueError("Expected one explicit package version")
    return text[:start] + section + text[end:]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bump", choices=("none", "patch", "minor", "major"))
    args = parser.parse_args()
    manifest = Path("Cargo.toml")
    original = manifest.read_text(encoding="utf-8")
    package = tomllib.loads(original)["package"]
    if package["name"] != "zterm":
        raise ValueError("Run this script from the zterm repository root")
    version = next_version(package["version"], args.bump)
    updated = replace_package_version(original, version)
    if tomllib.loads(updated)["package"]["version"] != version:
        raise ValueError("The updated manifest does not contain the new version")
    if version != package["version"]:
        manifest.write_text(updated, encoding="utf-8")
        subprocess.run(["cargo", "generate-lockfile", "--offline"], check=True)
    lock = tomllib.loads(Path("Cargo.lock").read_text(encoding="utf-8"))
    entries = [entry for entry in lock["package"] if entry["name"] == "zterm"]
    if len(entries) != 1 or entries[0]["version"] != version:
        raise ValueError("Cargo.lock does not match the candidate version")
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a", encoding="utf-8") as stream:
            stream.write(f"version={version}\n")
    print(f"Prepared zterm {version}")


if __name__ == "__main__":
    main()
