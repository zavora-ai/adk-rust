#!/usr/bin/env python3
"""Map a change to the workspace crates it can affect and the feature-coverage shards to run.

Reads the changed file paths on stdin and prints `crates=` and `shards=` lines for
$GITHUB_OUTPUT. A crate is affected when a file under it changed or when it depends,
directly or through other workspace crates, on one that did. Files under docs/ or
examples/, and Markdown and LICENSE files at the root, affect no crate. Any other file
outside the crates (the lockfile, a manifest, .cargo/, CI, scripts), or an empty list,
affects them all, and `crates=all` is printed.

Usage:
    git diff --name-only origin/main...HEAD | scripts/ci-scope.py
"""

import json
import subprocess
import sys
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
    if path.startswith(("docs/", "examples/")) or ("/" not in path and (path.endswith(".md") or path == "LICENSE")):
        continue
    name = crate_of(path)
    if name is None:
        everything = True
        break
    direct.add(name)

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

print("crates=" + ("all" if everything else " ".join(sorted(affected))))
print("shards=" + json.dumps(sorted(shards)))
print(
    f"{len(changed)} file(s) changed; {'every crate' if everything else f'{len(affected)} crate(s)'} "
    f"affected; feature-coverage shards {sorted(shards)}",
    file=sys.stderr,
)
