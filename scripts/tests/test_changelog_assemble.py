"""Tests for the changelog fragment assembly, the highlights section above all."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "changelog-assemble.py"
SPEC = importlib.util.spec_from_file_location("changelog_assemble", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
ASSEMBLER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ASSEMBLER)

CHANGELOG = """\
# Changelog

## [Unreleased]

## [3.0.0] - 2026-10-10

## [2.3.0] - 2026-10-09

### Fixed

- **Old fix**: unchanged.
"""


class AssembleTests(unittest.TestCase):
    """Assembles fragments in a temporary checkout."""

    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.fragments = self.root / "changelog.d"
        self.fragments.mkdir()
        self.changelog = self.root / "CHANGELOG.md"
        self.changelog.write_text(CHANGELOG)
        patches = [
            mock.patch.object(ASSEMBLER, "ROOT", self.root),
            mock.patch.object(ASSEMBLER, "FRAGMENT_DIR", self.fragments),
            mock.patch.object(ASSEMBLER, "CHANGELOG", self.changelog),
        ]
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)

    def fragment(self, name: str, text: str) -> None:
        (self.fragments / name).write_text(text)

    def test_highlights_is_a_valid_section(self) -> None:
        self.fragment("highlights.autonomous-agents.md", "- **Autonomous agents**: headline.\n")
        self.assertEqual(ASSEMBLER.validate(ASSEMBLER.fragments()), [])

    def test_release_notes_open_with_highlights(self) -> None:
        self.fragment("breaking.one.md", "- **Breaking one**: changed.\n")
        self.fragment("added.two.md", "- **Added two**: new.\n")
        self.fragment(
            "highlights.autonomous-agents.md",
            "- **Autonomous agents**: headline.\n  Continued.\n",
        )
        self.assertEqual(ASSEMBLER.assemble("3.0.0"), 0)
        self.assertEqual(
            self.changelog.read_text(),
            """\
# Changelog

## [Unreleased]

## [3.0.0] - 2026-10-10

### Highlights

- **Autonomous agents**: headline.
  Continued.

### Breaking

- **Breaking one**: changed.

### Added

- **Added two**: new.

## [2.3.0] - 2026-10-09

### Fixed

- **Old fix**: unchanged.
""",
        )
        self.assertEqual(list(self.fragments.iterdir()), [])

    def test_unknown_sections_are_still_refused(self) -> None:
        self.fragment("headline.one.md", "- **One**: text.\n")
        problems = ASSEMBLER.validate(ASSEMBLER.fragments())
        self.assertEqual(len(problems), 1)
        self.assertIn("highlights, breaking, security, added, changed, fixed", problems[0])


if __name__ == "__main__":
    unittest.main()
