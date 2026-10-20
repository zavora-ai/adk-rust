# Coding workflow as a graph — "ultra-review" style

A coding workflow expressed as an [`adk-graph`](../../adk-graph) `StateGraph`,
inspired by Claude Code's *ultracode / ultrareview* pattern: **implement → fan
out to parallel specialist reviewers → synthesize their verdicts → iterate until
they approve.**

```text
  START → implement ──┬─▶ review:correctness ─┐
                      ├─▶ review:edge-cases  ─┤   (3 real agents, in parallel)
                      └─▶ review:style       ─┤
                                               ▼
                                         synthesize   ← deferred fan-in (runs once)
                                               │
                            decision ──────────┤
                        ┌── "revise" ◀──────────┘
                        ▼                       └──▶ "finalize" → END
                     revise ─▶ (back to the three reviewers)
```

Every node is a **real agent** (no mocks): `implement`/`revise` are the
[`CodingAgent`](../../adk-harness) preset of `adk-harness` (read/write/bash in ask mode;
this unattended example approves each call through `ApproveAndLog` and prints it), and
each reviewer is a harness agent in **read-only** mode that inspects the code and returns
a `VERDICT: approve` or `VERDICT: changes` with notes. The CLI form is
`adk-rust ultracode`.

## What it demonstrates in adk-graph

- **Fan-out**: `implement` (and `revise`) have edges to three reviewer nodes that
  run **concurrently** in one super-step.
- **Fan-in barrier**: `synthesize` is a deferred node (`add_deferred_node_fn`) —
  it runs **exactly once**, after all three reviewers finish.
- **Conditional cyclic routing**: `synthesize` routes to `revise` (loop back to
  the reviewers) or `finalize`, bounded by a round cap + the graph recursion
  limit.

## Run

```bash
cargo run --manifest-path examples/coding_graph/Cargo.toml
```

Requires `GOOGLE_API_KEY` (Gemini 3, default) — or `CODING_PROVIDER=openai` with
`OPENAI_API_KEY`. Override the model with `CODING_MODEL`.

## What you'll see

A live trace of the workflow, then independent verification (the example imports
the produced `slugify` and checks it against the spec + edge cases):

```text
━━ implement ━━ writing the first version
  🔎 review:correctness → approve
  🔎 review:style       → changes — add a docstring …
  🔎 review:edge-cases  → changes — handle empty string …
━━ synthesize ━━ round 1: 2 change request(s) → revise
━━ revise ━━ applying review feedback
  🔎 review:style       → approve
  🔎 review:correctness → approve
  🔎 review:edge-cases  → approve
━━ synthesize ━━ round 2: all approved → finalize
══ verification ══
  slugify spec + edge cases: ✅ PASS
```
