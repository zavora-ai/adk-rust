"""Focused tests for the release-state markers parsed from README.md."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "check-release-consistency.py"
SPEC = importlib.util.spec_from_file_location("check_release_consistency", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)


class ReleaseStatePatternTests(unittest.TestCase):
    """Covers the two supported README publication states."""

    def test_candidate_markers_are_recognized(self) -> None:
        banner = CHECKER.README_BANNER_PATTERN.search(
            "> **v2.0.0 release candidate — unpublished.**"
        )
        roadmap = CHECKER.README_ROADMAP_PATTERN.search(
            "**v2.0.0** (release candidate)"
        )

        self.assertEqual(
            banner.groups(), ("2.0.0", "release candidate — unpublished.")
        )
        self.assertEqual(roadmap.groups(), ("2.0.0", "release candidate"))

    def test_released_markers_are_recognized(self) -> None:
        banner = CHECKER.README_BANNER_PATTERN.search(
            "> **🚀 v2.0.0 Released!**"
        )
        roadmap = CHECKER.README_ROADMAP_PATTERN.search("**v2.0.0** (current)")

        self.assertEqual(banner.groups(), ("2.0.0", "Released!"))
        self.assertEqual(roadmap.groups(), ("2.0.0", "current"))

    def test_ambiguous_markers_are_rejected(self) -> None:
        self.assertIsNone(
            CHECKER.README_BANNER_PATTERN.search("> **v2.0.0 available soon.**")
        )
        self.assertIsNone(
            CHECKER.README_ROADMAP_PATTERN.search("**v2.0.0** (next)")
        )


class ChangelogBoundaryTests(unittest.TestCase):
    """Covers the release headings and comparison links used at publication."""

    CHANGELOG = """\
## [Unreleased]

## [2.1.0] - 2026-08-25

## [2.0.0] - 2026-08-09

[Unreleased]: https://github.com/zavora-ai/adk-rust/compare/v2.1.0...HEAD
[2.1.0]: https://github.com/zavora-ai/adk-rust/compare/v2.0.0...v2.1.0
"""

    def test_release_headings_are_newest_first(self) -> None:
        self.assertEqual(
            CHECKER.changelog_release_headings(self.CHANGELOG),
            [("2.1.0", "2026-08-25"), ("2.0.0", "2026-08-09")],
        )

    def test_comparison_links_are_parsed(self) -> None:
        self.assertEqual(
            CHECKER.changelog_links(self.CHANGELOG),
            {
                "Unreleased": "https://github.com/zavora-ai/adk-rust/compare/v2.1.0...HEAD",
                "2.1.0": "https://github.com/zavora-ai/adk-rust/compare/v2.0.0...v2.1.0",
            },
        )


if __name__ == "__main__":
    unittest.main()
