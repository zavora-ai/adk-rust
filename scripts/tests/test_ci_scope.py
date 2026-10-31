"""Tests for scripts/ci-scope.py against the workspace's own manifests."""

from __future__ import annotations

import json
import subprocess
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "ci-scope.py"


def scope(*changed: str) -> tuple[str, list[int]]:
    output = subprocess.run(
        ["python3", str(SCRIPT)],
        input="\n".join(changed),
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    lines = dict(line.split("=", 1) for line in output.splitlines())
    return lines["crates"], json.loads(lines["shards"])


class CiScopeTests(unittest.TestCase):
    """Covers how changed files map to affected crates and feature-coverage shards."""

    def test_docs_and_examples_affect_no_crate(self) -> None:
        self.assertEqual(
            scope("docs/official_docs/index.md", "examples/website/src/main.rs", "README.md"),
            ("", []),
        )

    def test_a_file_outside_the_crates_affects_every_crate(self) -> None:
        crates, shards = scope("Cargo.lock")

        self.assertEqual(crates, "all")
        self.assertEqual(shards, list(range(11)))

    def test_an_empty_change_affects_every_crate(self) -> None:
        self.assertEqual(scope()[0], "all")

    def test_a_crate_brings_in_the_crates_that_depend_on_it(self) -> None:
        crates, shards = scope("adk-surface/src/lib.rs")

        self.assertIn("adk-surface", crates.split())
        self.assertIn("adk-cli", crates.split())
        self.assertNotIn("adk-core", crates.split())
        self.assertIn(9, shards)


if __name__ == "__main__":
    unittest.main()
