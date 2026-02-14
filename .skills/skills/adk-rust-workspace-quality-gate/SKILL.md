---
name: adk-rust-workspace-quality-gate
description: Run full ADK-Rust workspace verification and produce a findings-first review. Use when validating build/test/lint health, triaging regressions, or preparing merge/release quality reports.
---

# ADK Rust Workspace Quality Gate

## Overview
Run deterministic quality checks for this workspace and report issues by severity with file and line references.

## Execute Gate
1. Run `scripts/run_quality_gate.sh` from the repository root.
2. Inspect generated logs in `output/adk-quality/`.
3. Convert command output into a findings-first report.

## Reporting Rules
1. List findings first, highest severity to lowest.
2. Include exact file and line references for each finding.
3. Distinguish hard failures from ignored tests or external dependency skips.
4. If no findings exist, state that explicitly and list residual risk gaps.

## References
- Use `references/commands.md` for command matrix and severity rubric.
