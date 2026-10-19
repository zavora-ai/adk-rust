# Graph Workflows (ultra-review)

For anything beyond a single agent loop — parallel reviewers, adversarial checks,
multi-stage pipelines — orchestrate coding agents as an [`adk-graph`](../agents/graph-agents.md)
`StateGraph`. This is how the [`ultracode`](cli.md#ultracode--parallel-ultra-review)
command and the [`coding_graph` example](examples.md#coding_graph) work.

## The "ultra" pattern

```text
  START → implement ──┬─▶ review:correctness ─┐
                      ├─▶ review:edge-cases  ─┤   (parallel, real agents)
                      └─▶ review:style       ─┤
                                               ▼
                                         synthesize   ← deferred fan-in (runs once)
                                               │
                            decision ──────────┤
                        ┌── "revise" ◀──────────┘
                        ▼                       └──▶ "finalize" → END
                     revise ─▶ (back to the reviewers)
```

- **implement / revise** are full [`CodingAgent`](harness.md)s (read/write/bash).
- each **reviewer** is a read-only `LlmAgent` that inspects the code and returns a
  `VERDICT: approve` or `VERDICT: changes` with notes.
- **synthesize** aggregates the verdicts and routes to `revise` or `finalize`,
  bounded by a round cap + the graph recursion limit.

## Fan-out / fan-in in adk-graph

Two `adk-graph` behaviours make this correct:

- **Parallel fan-out** — several nodes reachable from one upstream node run
  **concurrently** in the same super-step.
- **Fan-in barrier** — a node with more than one incoming edge is deferred when the
  graph compiles: it runs **once**, after every upstream path that can still run in
  this pass arrives. `add_deferred_node_fn` (or `mark_deferred`) sets a
  `DeferredNodeConfig` explicitly, for a merge strategy, a timeout, or a quorum.

```rust
use adk_graph::edge::{END, START};
use adk_graph::error::Result;
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{NodeContext, NodeOutput};
use serde_json::json;

// In the coding_graph example, each of these runs an agent and writes its result.
async fn run_agent(_ctx: NodeContext) -> Result<NodeOutput> {
    Ok(NodeOutput::new())
}

// Aggregates the reviewers' verdicts into a decision.
async fn synthesize(ctx: NodeContext) -> Result<NodeOutput> {
    let approved = ["rev_correctness", "rev_edge-cases", "rev_style"]
        .iter()
        .all(|key| ctx.get(key).and_then(|v| v.as_str()).is_some_and(|v| v.contains("approve")));
    let decision = if approved { "finalize" } else { "revise" };
    Ok(NodeOutput::new().with_update("decision", json!(decision)))
}

fn ultra_review() -> Result<CompiledGraph> {
    Ok(StateGraph::with_channels(&["task", "round", "decision", "notes",
                                   "rev_correctness", "rev_edge-cases", "rev_style"])
        .add_node_fn("implement", run_agent)          // coding agent
        .add_node_fn("review_correctness", run_agent) // read-only reviewers
        .add_node_fn("review_edge-cases", run_agent)
        .add_node_fn("review_style", run_agent)
        .add_node_fn("synthesize", synthesize)        // fan-in: runs once per round
        .add_node_fn("revise", run_agent)             // coding agent applies notes
        .add_node_fn("finalize", run_agent)
        .add_edge(START, "implement")
        .add_edge("implement", "review_correctness")
        .add_edge("implement", "review_edge-cases")
        .add_edge("implement", "review_style")
        .add_edge("revise", "review_correctness")     // loop fans out again
        .add_edge("revise", "review_edge-cases")
        .add_edge("revise", "review_style")
        .add_edge("review_correctness", "synthesize") // fan-in
        .add_edge("review_edge-cases", "synthesize")
        .add_edge("review_style", "synthesize")
        .add_conditional_edges("synthesize",
            |s| s.get("decision").and_then(|v| v.as_str()).unwrap_or("finalize").to_string(),
            [("revise", "revise"), ("finalize", "finalize")])
        .add_edge("finalize", END)
        .compile()?
        .with_recursion_limit(16))
}
```

Each node runs an agent and writes its result into the graph state; the conditional
edge reads `decision` to loop or finish. See the full,
runnable implementation in the [`coding_graph` example](examples.md#coding_graph)
and the [Graph Agents](../agents/graph-agents.md) reference for the general API.

## When to use a graph vs. the harness

- **Single agent, linear/iterative work** → just a [`CodingAgent`](harness.md)
  (the `code` / `goal` commands).
- **Parallel specialists, multi-stage review, adversarial refutation, swarms** →
  an `adk-graph` workflow (the `ultracode` command).

Next: [Examples →](examples.md)
