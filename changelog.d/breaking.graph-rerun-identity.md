- **Subgraph and child runs are keyed by activation** (`adk-graph`): a `SubgraphNode` runs on
  `<parent thread>/<node name>@<parent step>` instead of `<parent thread>/<node name>`, and a
  `run_node` child is recorded under `<parent>@<step>/<child>@<run_id>` instead of
  `<parent>/<child>@<run_id>`. The record is dropped once the parent's step completes, and the
  default run id counts calls within one attempt of the parent. A run paused inside a subgraph
  before this release starts that subgraph again when resumed, and a paused or failed parent
  runs its finished children once more.
- **Fan-in arrivals and failures** (`adk-graph`): an entry of `{node}_fan_in` is the value the
  predecessor wrote to a channel named after itself, or else every update it wrote, instead of
  the whole state, so the channel no longer doubles on every pass of a loop.
  `GraphError::FanInIncomplete` fails the run when a join's remaining predecessors cannot run
  and fewer than its `min_predecessors` arrived.
