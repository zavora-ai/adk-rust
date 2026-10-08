#!/usr/bin/env python3
"""Assemble changelog fragments from changelog.d/ into CHANGELOG.md.

Every pull request records its user-facing change as one file under `changelog.d/`
instead of editing `CHANGELOG.md`, so parallel pull requests never conflict on the
same lines. A fragment is named `<section>.<slug>.md`, where `<section>` is one of
`breaking`, `security`, `added`, `changed`, or `fixed`, and contains one or more
Markdown list items exactly as they will appear under that heading.

Usage:
  python3 scripts/changelog-assemble.py --check
      Validate every fragment (name, section, list-item body). CI runs this.
  python3 scripts/changelog-assemble.py
      Move every fragment under `## [Unreleased]`, grouped by section in the
      canonical order, and delete the fragment files.
  python3 scripts/changelog-assemble.py --into 2.4.0
      Same, but into the `## [2.4.0] - YYYY-MM-DD` heading, for a release candidate.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FRAGMENT_DIR = ROOT / "changelog.d"
CHANGELOG = ROOT / "CHANGELOG.md"

SECTIONS = ["breaking", "security", "added", "changed", "fixed"]
HEADINGS = {s: f"### {s.capitalize()}" for s in SECTIONS}
NAME_PATTERN = re.compile(r"^(breaking|security|added|changed|fixed)\.[a-z0-9][a-z0-9-]*\.md$")


def fragments() -> list[Path]:
    """Every fragment file, excluding the directory's README."""
    if not FRAGMENT_DIR.is_dir():
        return []
    return sorted(p for p in FRAGMENT_DIR.iterdir() if p.suffix == ".md" and p.name != "README.md")


def validate(paths: list[Path]) -> list[str]:
    """Problems with the fragments, as human-readable lines."""
    problems: list[str] = []
    for path in paths:
        rel = path.relative_to(ROOT)
        if not NAME_PATTERN.match(path.name):
            problems.append(
                f"{rel}: name must be <section>.<slug>.md with section in "
                f"{', '.join(SECTIONS)} and a lowercase kebab-case slug"
            )
            continue
        text = path.read_text().strip("\n")
        if not text.strip():
            problems.append(f"{rel}: fragment is empty")
        elif not text.startswith("- "):
            problems.append(f"{rel}: fragment must start with a Markdown list item (`- `)")
        for line in text.split("\n"):
            if line and not line.startswith("- ") and not line.startswith("  "):
                problems.append(f"{rel}: continuation lines must be indented two spaces: {line[:60]!r}")
                break
    return problems


def items(block: str) -> list[str]:
    """Top-level list items in a changelog section body, continuation lines attached."""
    out: list[str] = []
    cur: list[str] = []
    for line in block.split("\n"):
        if line.startswith("- "):
            if cur:
                out.append("\n".join(cur).rstrip())
            cur = [line]
        elif line.strip() == "":
            if cur:
                out.append("\n".join(cur).rstrip())
                cur = []
        elif cur:
            cur.append(line)
    if cur:
        out.append("\n".join(cur).rstrip())
    return out


def assemble(into: str | None) -> int:
    paths = fragments()
    problems = validate(paths)
    if problems:
        print("\n".join(problems), file=sys.stderr)
        return 1
    if not paths:
        print("no fragments to assemble")
        return 0

    text = CHANGELOG.read_text()
    if into is None:
        heading_re = re.compile(r"^## \[Unreleased\][ \t]*$", re.MULTILINE)
    else:
        heading_re = re.compile(rf"^## \[{re.escape(into)}\] - \d{{4}}-\d{{2}}-\d{{2}}[ \t]*$", re.MULTILINE)
    heading = heading_re.search(text)
    if heading is None:
        target = "## [Unreleased]" if into is None else f"## [{into}] - YYYY-MM-DD"
        print(f"CHANGELOG.md has no `{target}` heading", file=sys.stderr)
        return 1
    next_release = re.compile(r"^## \[", re.MULTILINE).search(text, heading.end())
    start, end = heading.end(), next_release.start() if next_release else len(text)

    # Existing entries by section, then the fragments appended in file order.
    groups: dict[str, list[str]] = {HEADINGS[s]: [] for s in SECTIONS}
    current: str | None = None
    for line in text[start:end].split("\n"):
        if line.startswith("### "):
            current = line.strip()
            groups.setdefault(current, [])
            continue
        if current is not None:
            groups[current].append(line)
    merged: dict[str, list[str]] = {}
    for key, body in groups.items():
        merged[key] = items("\n".join(body))
    for path in paths:
        section = HEADINGS[path.name.split(".", 1)[0]]
        for item in items(path.read_text().strip("\n")):
            if item not in merged[section]:
                merged[section].append(item)

    ordered = [*HEADINGS.values(), *[k for k in merged if k not in HEADINGS.values()]]
    blocks = [key + "\n\n" + "\n\n".join(merged[key]) for key in ordered if merged.get(key)]
    CHANGELOG.write_text(text[:start] + "\n\n" + "\n\n".join(blocks) + "\n\n" + text[end:])

    for path in paths:
        path.unlink()
    where = "[Unreleased]" if into is None else f"[{into}]"
    print(f"assembled {len(paths)} fragment(s) into {where} and removed them from changelog.d/")
    return 0


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="validate fragments without touching CHANGELOG.md")
    parser.add_argument("--into", metavar="VERSION", help="assemble into the dated `## [VERSION]` heading instead of Unreleased")
    args = parser.parse_args()

    if args.check:
        problems = validate(fragments())
        if problems:
            print("\n".join(problems), file=sys.stderr)
            sys.exit(1)
        print(f"changelog.d: {len(fragments())} fragment(s) valid")
        return
    sys.exit(assemble(args.into))


if __name__ == "__main__":
    main()
