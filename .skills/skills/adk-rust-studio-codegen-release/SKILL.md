---
name: adk-rust-studio-codegen-release
description: Use ADK Studio code generation and perform release-readiness checks for ADK-Rust changes. Use when validating generated projects, examples, and release quality gates.
---

# ADK Rust Studio Codegen Release

## Overview
Validate Studio codegen output and release readiness with deterministic checks.

## Workflow
1. Run Studio codegen demo and inspect generated files.
2. Compile generated output for syntax and dependency sanity.
3. Run workspace quality gate before release actions.
4. Summarize release blockers and risk by severity.

## Guardrails
1. Treat generated code as source requiring checks.
2. Validate feature flags for generated templates.
3. Keep release notes aligned with verified behavior.

## References
- Use `references/studio-release-checklist.md`.
