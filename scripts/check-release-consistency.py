#!/usr/bin/env python3
"""Check that every release statement agrees with the workspace version and state.

The workspace version in the root `Cargo.toml` is the single source. The changelog
heading, README banner, and README roadmap marker are checked against it, so a
version bump cannot land with any of them stale. The README may describe the
workspace as an unpublished release candidate before publication or as released
after publication, but the banner and roadmap must describe the same state.

`scripts/check-doc-versions.py` covers dependency snippets and feature names; it
explicitly skips `CHANGELOG.md` and does not look at the banner or the roadmap, which
is how those three drifted apart.

Release mode (`--release`) additionally requires an annotated `v<version>` tag so a
published artifact can be attributed to an exact commit. The normal pull-request
gate validates documentation state without requiring tags, because pull-request
checkouts may be shallow.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
README_BANNER_PATTERN = re.compile(
    r"\*\*(?:🚀\s*)?v(\d+\.\d+\.\d+)\s+"
    r"(release candidate\s+—\s+unpublished\.|Released!)\*\*",
    re.IGNORECASE,
)
README_ROADMAP_PATTERN = re.compile(
    r"\*\*v(\d+\.\d+\.\d+)\*\*\s*\((release candidate|current)\)",
    re.IGNORECASE,
)


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


def readme_banner() -> tuple[str, str] | None:
    """The version and publication state announced by the README banner."""
    match = README_BANNER_PATTERN.search((ROOT / "README.md").read_text())
    if not match:
        return None
    state = (
        "candidate"
        if match.group(2).lower().startswith("release candidate")
        else "released"
    )
    return match.group(1), state


def readme_roadmap() -> tuple[str, str] | None:
    """The version and publication state marked by the README roadmap."""
    match = README_ROADMAP_PATTERN.search((ROOT / "README.md").read_text())
    if not match:
        return None
    state = "candidate" if match.group(2).lower() == "release candidate" else "released"
    return match.group(1), state


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
        help="also require an annotated v<version> tag, for use when cutting a release",
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

    banner = readme_banner()
    if banner is None:
        failures.append(
            "README.md has neither a `vX.Y.Z release candidate — unpublished.` "
            "nor a `vX.Y.Z Released!` banner"
        )
    elif banner[0] != version:
        failures.append(
            f"README.md's release banner announces v{banner[0]}, "
            f"but the workspace version is {version}"
        )

    roadmap = readme_roadmap()
    if roadmap is None:
        failures.append(
            "README.md's roadmap has neither a `**vX.Y.Z** (release candidate)` "
            "nor a `**vX.Y.Z** (current)` marker"
        )
    elif roadmap[0] != version:
        failures.append(
            f"README.md's roadmap marks v{roadmap[0]}, "
            f"but the workspace version is {version}"
        )
    if banner is not None and roadmap is not None and banner[1] != roadmap[1]:
        failures.append(
            f"README.md's banner is {banner[1]}, but its roadmap is {roadmap[1]}"
        )

    tag = f"v{version}"
    tag_type = git("cat-file", "-t", tag)
    tag_commit = git("rev-list", "-n", "1", tag)
    if tag_type == "tag" and tag_commit:
        print(f"release baseline: {tag} -> {tag_commit}")
    elif tag_type:
        if args.release:
            failures.append(
                f"{tag} is a {tag_type} object, not an annotated tag. Replace it with: "
                f"git tag -a {tag} -m 'Release {version}'"
            )
        else:
            print(
                f"note: {tag} exists but is not annotated; release mode will reject it"
            )
    elif args.release:
        failures.append(
            f"no annotated {tag} tag exists, so the release cannot be attributed to a commit. "
            f"Create it with: git tag -a {tag} -m 'Release {version}'"
        )
    else:
        head = git("rev-parse", "HEAD") or "unknown"
        print(
            f"note: no {tag} tag yet; a release from this commit would be {head}. "
            f"Run with --release to make this a failure."
        )

    if banner is not None and banner[1] == "candidate":
        print(f"release state: v{version} is an unpublished release candidate")
        if tag_type == "tag":
            print(
                "post-publication transition: after every workspace crate is available "
                "on crates.io, change the README banner to `Released!` and the roadmap "
                "marker to `(current)`"
            )
    elif banner is not None:
        print(f"release state: v{version} is marked released")

    if failures:
        print(
            f"\nrelease statements disagree with the workspace version or state "
            f"({version}):\n"
        )
        for failure in failures:
            print(f"  - {failure}")
        sys.exit(1)

    print(f"release statements agree on {version} and its publication state")


if __name__ == "__main__":
    main()
