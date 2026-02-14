---
name: adk-rust-graph-orchestration
description: Build and debug ADK graph workflows with checkpoints, routing, interrupts, and state reducers. Use when implementing graph-based orchestration in adk-graph.
---

# ADK Rust Graph Orchestration

## Overview
Use `StateGraph` for explicit state channels and deterministic routing.

## Workflow
1. Define schema channels and reducer semantics.
2. Add nodes and explicit entry/exit edges.
3. Add conditional routes with explicit targets.
4. Add checkpoint/interrupt behavior only after base flow is stable.
5. Verify recursion limits and stop conditions.

## Guardrails
1. Validate every edge target against known nodes.
2. Keep routing keys small and deterministic.
3. Emit structured events for stream-mode debugging.

## References
- Use `references/graph-playbook.md`.
