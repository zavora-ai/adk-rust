"""Tests for scripts/ci-scope.py against the workspace's own manifests."""

from __future__ import annotations

import json
import re
import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "ci-scope.py"
SCOPED_JOBS = [
    "postgres",
    "templates",
    "kernel-scenarios",
    "website-scenarios",
    "vision-scenarios",
    "windows",
    "surface-e2e",
]


def run_scope(*changed: str) -> dict[str, str]:
    output = subprocess.run(
        ["python3", str(SCRIPT)],
        input="\n".join(changed),
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return dict(line.split("=", 1) for line in output.splitlines())


def scope(*changed: str) -> tuple[str, list[int]]:
    lines = run_scope(*changed)
    return lines["crates"], json.loads(lines["shards"])


def jobs(*changed: str) -> list[str]:
    return json.loads(run_scope(*changed)["jobs"])


def job_rows() -> dict[str, set[str]]:
    rows = {}
    for line in (ROOT / "scripts" / "ci-job-crates.txt").read_text().splitlines():
        fields = line.split()
        if fields and not fields[0].startswith("#"):
            rows[fields[0]] = set(fields[1:])
    return rows


def workflow_job(name: str) -> str:
    """The text of one job of ci.yml, from its key to the next job's."""
    workflow = (ROOT / ".github" / "workflows" / "ci.yml").read_text()
    body = workflow.split(f"\n  {name}:\n", 1)[1]
    return re.split(r"\n  [a-z][a-z-]*:\n", body, maxsplit=1)[0]


def packages(text: str) -> set[str]:
    """The packages `text` passes to cargo with `-p`."""
    return set(re.findall(r"(?<![\w-])-p ([a-z][a-z0-9-]*)", text))


class CiScopeTests(unittest.TestCase):
    """Covers how changed files map to affected crates and feature-coverage shards."""

    def test_docs_and_examples_affect_no_crate(self) -> None:
        self.assertEqual(
            scope("docs/official_docs/index.md", "examples/website/src/main.rs", "README.md"),
            ("", []),
        )

    def test_changelog_fragments_and_the_env_example_affect_no_crate(self) -> None:
        self.assertEqual(
            scope("changelog.d/added.example.md", ".env.example", "CONTRIBUTING.md"),
            ("", []),
        )

    def test_a_changelog_fragment_keeps_a_crate_change_scoped(self) -> None:
        self.assertEqual(
            scope("adk-surface/src/lib.rs", "changelog.d/fixed.example.md"),
            scope("adk-surface/src/lib.rs"),
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

    def test_docs_run_no_scoped_job(self) -> None:
        self.assertEqual(jobs("docs/official_docs/index.md", "README.md"), [])

    def test_a_file_outside_the_crates_runs_every_scoped_job(self) -> None:
        self.assertEqual(jobs("Cargo.lock"), SCOPED_JOBS)
        self.assertEqual(jobs(), SCOPED_JOBS)

    def test_a_core_change_runs_every_scoped_job(self) -> None:
        self.assertEqual(jobs("adk-core/src/lib.rs"), SCOPED_JOBS)

    def test_a_surface_change_skips_the_jobs_that_do_not_build_it(self) -> None:
        # examples/vision drives the live camera surface (spec 3.7 S12).
        self.assertEqual(
            jobs("adk-surface/src/lib.rs"),
            ["templates", "vision-scenarios", "windows", "surface-e2e"],
        )

    def test_an_example_change_runs_only_that_examples_scenarios(self) -> None:
        self.assertEqual(jobs("examples/website/src/main.rs"), ["website-scenarios"])
        self.assertEqual(jobs("examples/harness_kernel/Cargo.toml"), ["kernel-scenarios"])
        self.assertEqual(jobs("examples/vision/src/main.rs"), ["vision-scenarios"])
        self.assertEqual(jobs("examples/desktop_audio/src/main.rs"), [])

    def test_an_example_runs_when_a_crate_it_names_by_path_changes(self) -> None:
        self.assertEqual(
            jobs("adk-website/src/lib.rs"),
            ["templates", "website-scenarios", "vision-scenarios", "windows", "surface-e2e"],
        )

    def test_every_scoped_job_has_a_row_and_a_workflow_condition(self) -> None:
        self.assertEqual(list(job_rows()), SCOPED_JOBS)
        for job in SCOPED_JOBS:
            self.assertIn(f"contains(fromJSON(needs.scope.outputs.jobs), '{job}')", workflow_job(job))

    def test_each_row_names_the_packages_its_job_builds(self) -> None:
        rows = job_rows()
        scripts = ROOT / "scripts"

        self.assertEqual(
            rows["postgres"],
            packages((scripts / "test-postgres.sh").read_text()) | packages(workflow_job("postgres")),
        )
        self.assertEqual(rows["windows"], packages(workflow_job("windows")))
        self.assertEqual(
            rows["surface-e2e"],
            packages((scripts / "check-surface-api.sh").read_text())
            | packages((ROOT / "adk-surface" / "web" / "playwright.config.ts").read_text()),
        )
        templates = (scripts / "check-cargo-adk-templates.sh").read_text()
        self.assertEqual(
            rows["templates"],
            packages(templates) | set(re.findall(r'path = "\$ROOT/([a-z0-9-]+)"', templates)),
        )
        self.assertEqual(rows["kernel-scenarios"], {"examples/harness_kernel"})
        self.assertEqual(rows["website-scenarios"], {"examples/website"})
        self.assertEqual(rows["vision-scenarios"], {"examples/vision"})


if __name__ == "__main__":
    unittest.main()
