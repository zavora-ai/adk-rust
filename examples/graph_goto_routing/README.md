# Routing chosen by the model

A support desk where an `LlmAgent` classifies a ticket and names the node that
handles it. The graph declares **no edge out of the classifier**.

```text
  START ─→ classify ┄┄┄→ refund_desk  ─→ END
                    ┄┄┄→ tech_desk    ─→ END
                    ┄┄┄→ billing_desk ─→ END
         (dotted: chosen at run time, not declared)
```

This is the counterpart to LangGraph's `Command(update=..., goto=...)`. Their docs
make the same observation about the resulting graph: it carries no conditional
edge for the routing, because the node decides.

## Why not a conditional edge

`add_conditional_edges` maps a route key to a target chosen when the graph is
built, and evaluates a router function against state. That works when the branch
set is fixed and the decision is separable from the node.

Here the classifier writes its category and names the desk in the same step, from
the same answer, so the two cannot disagree. `AgentNode::with_goto_mapper` reads
the updates the output mapper produced and returns the targets:

```rust
fn desk_for(updates: &HashMap<String, Value>) -> Option<Vec<String>> {
    let category = updates.get("category")?.as_str()?;
    DESKS.iter()
        .find(|(answer, _)| category.contains(answer))
        .map(|(_, desk)| vec![(*desk).to_string()])
}
```

Returning `None` leaves the declared edges in charge. The classifier has none, so
an unrecognised answer ends the run with no desk reached.

## Run it

```bash
export OPENAI_API_KEY=sk-...
cargo run --manifest-path examples/graph_goto_routing/Cargo.toml
```

Set `GRAPH_MODEL` to use a different model. The default is `gpt-5-mini`.

## Output

```
model: gpt-5-mini

ticket 1: I was charged twice for the same order and want my money back.
  model answered: refund
  desk reached:   refund_desk

ticket 2: The app crashes every time I open the settings screen.
  model answered: technical
  desk reached:   tech_desk

ticket 3: Can you explain the line items on my October statement?
  model answered: billing
  desk reached:   billing_desk
```

Remove the `with_goto_mapper` call and every ticket reports `(no desk ran)`,
because nothing else can reach a desk.

> **Note:** a goto naming a node the graph does not hold fails the run with
> `GraphError::UnknownRouteTarget`, so a model that answers with something
> unexpected cannot route to a node that is not there.
