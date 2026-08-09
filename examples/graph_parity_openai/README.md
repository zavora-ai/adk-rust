# Graph parity over the OpenAI API

A code-review pipeline that exercises four capabilities of `adk-graph` in one
run, against `gpt-5-mini`.

| Capability | Where it shows |
|------------|----------------|
| Imperative child invocation | `fan_out` reads the planner's answer and invokes the reviewer once per aspect |
| Bounded concurrency | the graph runs at most two nodes at once |
| Per-node retry with backoff | the planner and the reviewer each retry a transient failure three times |
| Interrupt and resume | one dynamic pause inside `fan_out`, one static pause in front of `publish` |

## Why declared edges are not enough here

The number of reviewers is decided by a model at run time. A graph built before
the run cannot hold an edge per reviewer, because the count is unknown. `fan_out`
therefore invokes the reviewer node directly, once per aspect, and gives each
call a run id — the aspect name.

That run id is what makes the pause cheap. Every completed child is recorded
under `fan_out/reviewer@<aspect>`. When a person approves and the thread runs
again, `fan_out` re-runs from the top and asks for every review a second time,
but the recorded answers are returned instead of the model being called again.

## Run it

```bash
export OPENAI_API_KEY=sk-...
cargo run --manifest-path examples/graph_parity_openai/Cargo.toml
```

Set `GRAPH_MODEL` to use a different model. The default is `gpt-5-mini`.

## What the output proves

The example counts reviewer calls that reached the model. The counter sits in the
output mapper, which runs only when the agent ran, so an answer served from the
ledger does not move it.

```
=== First run: plan, review, then stop at the approval gate ===
  planner chose 2 aspect(s): correctness, performance
  correctness: ... — FAIL
  performance: ... — FAIL
  paused: Dynamic interrupt: approve publishing this review?
  reviewer model calls so far: 2

=== A person approves, so the same thread runs again ===
  planner chose 2 aspect(s): correctness, performance
  correctness: ... — FAIL
  performance: ... — FAIL
  pause 1: Interrupt before 'publish'
  verdict: Some(String("2 concern(s)"))
  reviewer model calls in total: 2
```

The reviews print twice because `fan_out` asks for them twice. The call total
does not grow, which is the part that matters. With the ledger recording removed,
the same run reports 3 calls and then 6.

> **Note:** the aspect names come from a model, so the run is not identical every
> time. The planner may choose one, two, or three aspects.
