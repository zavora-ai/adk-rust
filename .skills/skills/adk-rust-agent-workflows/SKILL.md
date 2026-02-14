---
name: adk-rust-agent-workflows
description: Design and implement ADK-Rust agent workflow patterns including LLM, sequential, parallel, loop, and multi-agent orchestration. Use when building or refactoring agent topology.
---

# ADK Rust Agent Workflows

## Overview
Implement the simplest correct workflow topology for the target behavior.

## Workflow
1. Start from single `LlmAgent`.
2. Escalate to `SequentialAgent` for ordered deterministic stages.
3. Use `ParallelAgent` only when stages are independent.
4. Use `LoopAgent` with explicit exit conditions and max-iteration guard.
5. Validate with focused workflow tests.

## Guardrails
1. Keep tool boundaries explicit per agent.
2. Avoid hidden cross-agent state coupling.
3. Add tests for transfer, callback order, and failure behavior.

## References
- Use `references/workflow-patterns.md`.
