#!/usr/bin/env python3
"""Map a change to the workspace crates it can affect and the PR-tier jobs to run.

Reads the changed file paths on stdin and prints `crates=`, `shards=` and `jobs=` lines
for $GITHUB_OUTPUT. A crate is affected when a file under it changed or when it depends,
directly or through other workspace crates, on one that did. Files under docs/ or
examples/, and Markdown (changelog.d/ included), LICENSE and .env.example outside the
crates, affect no crate. Any other file outside the crates (the lockfile, a manifest,
.cargo/, CI, scripts), or an empty list, affects them all, and `crates=all` is printed.

`shards` lists the feature-coverage shards holding a pair of an affected crate
(scripts/feature-coverage-pairs.txt). `jobs` lists the jobs of scripts/ci-job-crates.txt
that build an affected crate or a changed standalone example; a job whose example manifest
cannot be read, or names a path outside the workspace crates, runs.

Usage:
    git diff --name-only origin/main...HEAD | scripts/ci-scope.py
"""

import json
import os
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

changed = [line.strip() for line in sys.stdin if line.strip()]
metadata = json.loads(
    subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
)
crates = {package["name"]: package for package in metadata["packages"]}
crate_dirs = {
    Path(package["manifest_path"]).parent.relative_to(ROOT).as_posix(): name
    for name, package in crates.items()
}

dependents = {name: set() for name in crates}
for name, package in crates.items():
    for dependency in package["dependencies"]:
        if dependency.get("path") and dependency["name"] in crates:
            dependents[dependency["name"]].add(name)


def crate_of(path):
    for directory, name in crate_dirs.items():
        if directory != "." and (path == directory or path.startswith(directory + "/")):
            return name
    return None


everything = not changed
direct = set()
for path in changed:
    if path.startswith(("docs/", "examples/")):
        continue
    name = crate_of(path)
    if name is not None:
        direct.add(name)
    elif not (path.endswith(".md") or path in ("LICENSE", ".env.example")):
        everything = True
        break

if everything:
    affected = set(crates)
else:
    affected, frontier = set(direct), list(direct)
    while frontier:
        for dependent in dependents[frontier.pop()]:
            if dependent not in affected:
                affected.add(dependent)
                frontier.append(dependent)

shards = set()
for line in (ROOT / "scripts" / "feature-coverage-pairs.txt").read_text().splitlines():
    fields = line.split()
    if fields and not fields[0].startswith("#") and fields[1] in affected:
        shards.add(int(fields[0]))



def example_runs(directory):
    """Whether a change reaches the standalone example in `directory`."""
    if any(path.startswith(directory + "/") for path in changed):
        return True
    try:
        manifest = tomllib.loads((ROOT / directory / "Cargo.toml").read_text())
    except (OSError, tomllib.TOMLDecodeError):
        return True
    for table in ("dependencies", "dev-dependencies", "build-dependencies"):
        for dependency in manifest.get(table, {}).values():
            if not isinstance(dependency, dict) or "path" not in dependency:
                continue
            target = os.path.normpath(os.path.join(directory, dependency["path"]))
            name = crate_dirs.get(Path(target).as_posix())
            if name is None or name in affected:
                return True
    return False


jobs = []
for line in (ROOT / "scripts" / "ci-job-crates.txt").read_text().splitlines():
    fields = line.split()
    if not fields or fields[0].startswith("#"):
        continue
    if everything or any(
        example_runs(entry) if "/" in entry else entry in affected for entry in fields[1:]
    ):
        jobs.append(fields[0])

print("crates=" + ("all" if everything else " ".join(sorted(affected))))
print("shards=" + json.dumps(sorted(shards)))
print("jobs=" + json.dumps(jobs))
print(
    f"{len(changed)} file(s) changed; {'every crate' if everything else f'{len(affected)} crate(s)'} "
    f"affected; feature-coverage shards {sorted(shards)}; scoped jobs {jobs}",
    file=sys.stderr,
)
