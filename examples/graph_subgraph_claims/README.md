# Claim pipeline built from nested graphs

An insurance claim handled by three graphs, two of them nested. Each is written on
its own: `pricing` knows nothing about claims, and `assessment` knows nothing about
the settlement it feeds.

```text
 claim_pipeline   START ─→ assess ──→ settle ─→ END
                             │  ╰┄┄┄→ escalate ─→ END      (decided inside)
                             │
 assessment       START ─→ classify ─→ estimate ─→ decide ─→ END
                                         │
 pricing          START ─→ quote ─→ [adjuster sign-off] ─→ commit ─→ END
```

| Feature | Where it shows |
|---------|----------------|
| Channel mapping | `claim_text` → `text` → `description`, and the estimate back out as `amount` |
| `isolated()` | both boundaries name every exchange, so adding a channel to one graph cannot silently feed another |
| A pause two graphs deep | the adjuster gate inside `pricing` |
| Resuming that pause | the second run finishes without paying for the model again |
| Handing control to the parent | `decide` escalates a vague claim to a node it has no edge to |
| Graph-wide defaults | one `RetryPolicy` for every model call, stated once |

## Run it

```bash
export OPENAI_API_KEY=sk-...
cargo run --manifest-path examples/graph_subgraph_claims/Cargo.toml
```

Set `GRAPH_MODEL` to use a different model. The default is `gpt-5-mini`.

## Output

```
=== A claim the model can price ===

claim: Rear bumper and tail light damaged when another car reversed into my parked
       Toyota Corolla in a supermarket car park.
  paused:  Dynamic interrupt: assess: estimate: Interrupt before 'commit'
  model calls so far: 2
  category: Some("motor")
  amount:   Some("1200")
  outcome:  Some("SETTLED — settle at 1200")
  model calls in total: 2 (unchanged, so nothing was re-priced)

=== A claim it cannot, escalated from inside the assessment ===

claim: Something happened to my stuff. Please help.
  paused:  Dynamic interrupt: assess: estimate: Interrupt before 'commit'
  model calls so far: 2
  category: Some("property")
  amount:   Some("UNCLEAR")
  outcome:  Some("ESCALATED to a human — no usable estimate")
  model calls in total: 2 (unchanged, so nothing was re-priced)
```

Two things in that output are worth reading closely.

**The pause message names every level it passed through** — `assess: estimate:
Interrupt before 'commit'` — so a gate deep in a composition says where it is.

**The call total does not grow across the resume.** The counter sits in each
agent's output mapper, which runs only when the agent ran, so a figure served from
the subgraph's checkpoint does not move it. Make the subgraph's thread unstable and
the same run never completes: the resume re-enters a fresh subgraph, pauses again,
and the second invocation returns the interrupt as an error.

**The second claim never reaches `settle`.** `decide` is two graphs down and has no
edge to anything in the top graph, but `with_goto_parent(["escalate"])` sends the
claim there. The top graph validates the name, because it is the only side that
knows its own nodes.

## What fails before it runs

A subgraph mapping a channel neither side declares fails when the **parent
compiles**, naming the channel and the side. So does a subgraph that declares an
interrupt gate without a checkpointer — it would re-enter at its first node on
resume and pay for its finished work again.

> **Note:** the model decides the category and the estimate, so the figures vary
> between runs. A vague claim is not guaranteed to produce `UNCLEAR`, though it
> usually does.
