---
name: adk-studio
description: Build, debug, and validate ADK Studio projects and generated Rust workflows. Use when working on adk-studio schemas, action nodes, code generation, graph runner behavior, or studio deployment readiness.
---

# ADK Studio

## Overview
Use this skill for end-to-end ADK Studio work: schema editing, codegen verification, graph runtime behavior, and release gating.

## Workflow
1. Validate schema intent before editing node graphs.
2. Update schema/action-node logic in `adk-studio` with minimal deltas.
3. Run Studio-focused tests first, then workspace quality gates.
4. Run code generation demo and inspect generated artifacts.
5. Summarize failures with exact file paths and test names.

## Studio Debug Checklist
1. Verify node connectivity and router target validity.
2. Verify interrupt/resume behavior for graph runner paths.
3. Verify generated Rust compiles with expected feature flags.
4. Verify env var validation for tools/providers.

## Output Expectations
1. Findings-first report with severity and impact.
2. Exact command log summary for tests/checks run.
3. Clear separation between Studio issues and workspace-wide issues.

## References
- Use `references/studio-workflow.md` for command order and debugging patterns.
