#!/usr/bin/env python3
"""Check that every release statement agrees with the workspace version.

The workspace version in the root `Cargo.toml` is the single source. The changelog
heading, the README release banner, and the README roadmap's "current" marker are
checked against it, so a version bump cannot land with any of them stale.

`scripts/check-doc-versions.py` covers dependency snippets and feature names; it
explicitly skips `CHANGELOG.md` and does not look at the banner or the roadmap, which
is how those three drifted apart.

Release mode (`--release`) additionally requires an annotated `v<version>` tag so a
published artifact can be attributed to an exact commit. Without it the run reports the
missing tag and the commit that would be tagged, but does not fail — a tag is a release
action, not a pull-request gate.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def workspace_version() -> str:
    """The version every other statement is checked against."""
    with (ROOT / "Cargo.toml").open("rb") as handle:
        manifest = tomllib.load(handle)
    return manifest["workspace"]["package"]["version"]


def changelog_heading_version() -> tuple[str, str] | None:
    """The version and date of the topmost changelog release heading."""
    pattern = re.compile(r"^## \[(\d+\.\d+\.\d+)\]\s*-\s*(\d{4}-\d{2}-\d{2})\s*$")
    for line in (ROOT / "CHANGELOG.md").read_text().splitlines():
        match = pattern.match(line)
        if match:
            return match.group(1), match.group(2)
    return None


def readme_banner_version() -> str | None:
    """The version announced by the README release banner."""
    match = re.search(r"v(\d+\.\d+\.\d+) Released!", (ROOT / "README.md").read_text())
    return match.group(1) if match else None


def readme_roadmap_current_version() -> str | None:
    """The version the README roadmap marks as current."""
    match = re.search(
        r"\*\*v(\d+\.\d+\.\d+)\*\*\s*\(current\)", (ROOT / "README.md").read_text()
    )
    return match.group(1) if match else None


def git(*args: str) -> str:
    """Runs git and returns stdout, or an empty string when git is unavailable."""
    try:
        return subprocess.run(
            ["git", *args], cwd=ROOT, capture_output=True, text=True, check=True
        ).stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return ""


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--release",
        action="store_true",
        help="also require a v<version> tag, for use when cutting a release",
    )
    args = parser.parse_args()

    version = workspace_version()
    failures: list[str] = []

    heading = changelog_heading_version()
    if heading is None:
        failures.append("CHANGELOG.md has no `## [x.y.z] - YYYY-MM-DD` release heading")
    elif heading[0] != version:
        failures.append(
            f"CHANGELOG.md's newest release heading is {heading[0]}, "
            f"but the workspace version is {version}"
        )

    banner = readme_banner_version()
    if banner is None:
        failures.append("README.md has no `vX.Y.Z Released!` banner")
    elif banner != version:
        failures.append(
            f"README.md's release banner announces v{banner}, "
            f"but the workspace version is {version}"
        )

    roadmap = readme_roadmap_current_version()
    if roadmap is None:
        failures.append("README.md's roadmap has no `**vX.Y.Z** (current)` marker")
    elif roadmap != version:
        failures.append(
            f"README.md's roadmap marks v{roadmap} as current, "
            f"but the workspace version is {version}"
        )

    tag = f"v{version}"
    tag_commit = git("rev-list", "-n", "1", tag)
    if tag_commit:
        print(f"release baseline: {tag} -> {tag_commit}")
    elif args.release:
        failures.append(
            f"no {tag} tag exists, so the release cannot be attributed to a commit. "
            f"Create it with: git tag -a {tag} -m 'Release {version}'"
        )
    else:
        head = git("rev-parse", "HEAD") or "unknown"
        print(
            f"note: no {tag} tag yet; a release from this commit would be {head}. "
            f"Run with --release to make this a failure."
        )

    if failures:
        print(f"\nrelease statements disagree with the workspace version ({version}):\n")
        for failure in failures:
            print(f"  - {failure}")
        sys.exit(1)

    print(f"release statements agree on {version}")


if __name__ == "__main__":
    main()
