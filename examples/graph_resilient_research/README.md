# A research desk the model calls as a tool

An `LlmAgent` is given one tool. Behind it is a graph: three sources queried at
once, joined into a single summary, with a source that is down recorded rather than
ending the run.

```text
 LlmAgent ──tool──→ research graph

   START ─→ fast_source    ─┐
         ─→ careful_source ─┼─→ synthesise ─→ END
         ─→ broken_source  ─┘
```

| Feature | Where it shows |
|---------|----------------|
| A graph as a tool | `NodeTool::for_graph`, handed to the agent like any other tool |
| Failure recovery | `with_node_error_handler` records the 503 and lets the desk finish |
| Fan-in runs once | `synthesise` has three incoming edges and runs once, after all three arrive |
| Channel enforcement | `with_strict_channels`, so a mistyped channel fails the run |
| Time travel | the checkpoint history is read back after the run |

The parameter schema the model sees is derived from the graph's own channels, so
the tool description and the graph cannot drift apart.

## Run it

```bash
export OPENAI_API_KEY=sk-...
cargo run --manifest-path examples/graph_resilient_research/Cargo.toml
```

Set `GRAPH_MODEL` to use a different model. The default is `gpt-5-mini`.

## Output

```
tool advertised to the model: research_desk

=== The agent decides to call the graph ===

  answer: The research desk found that pair programming generally reduces
  defects... One source was unavailable (archive index 503), so the desk used
  two sources out of three.

  tool calls the model made: 1
  the desk answered in 21.1s

=== What the desk recorded ===

  sources that answered: 2 of 3
  source_errors: broken_source: archive index unavailable (503)

=== Reading the desk's history back ===

  step 1: 1 node(s) pending
  step 2: 0 node(s) pending
```

Three things in that output are the point.

**`tool calls the model made: 1`** — the model chose to call the graph. It never
sees nodes, edges or channels; the graph is one tool with one parameter.

**`sources that answered: 2 of 3`, with the third named as an error.** `broken_source`
fails every time. Without a handler that ends the whole question; with one, the
failure becomes state the summary can mention, and the agent passes it on to the
user unprompted.

**The desk's history is readable afterwards.** Every super-step left a checkpoint,
so `time_travel` can list the steps, read the state at one, or fork it.

> **Note:** a slow node holds its own super-step. `min_predecessors` decides how
> many arrivals admit a deferred node to the *next* frontier, so it helps when
> branches span different numbers of steps — it does not outrun a slow sibling
> within one step, because the join is evaluated only after that step ends.
