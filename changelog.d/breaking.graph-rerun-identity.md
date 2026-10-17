- **Graph work is keyed by activation** (`adk-graph`):
  - A `SubgraphNode` runs on `<parent thread>/<node name>@<parent step>` instead of
    `<parent thread>/<node name>`.
  - `NodeTool` runs each graph call on
    `adk_graph::agent::tool_call_thread_id(app_name, user_id, session_id, function_call_id)`
    instead of the session's thread.
  - A `run_node` child is recorded under `<parent>@<step>/<child>@<run_id>`, the record is
    dropped once the parent's step completes, and the default run id counts calls within one
    attempt of the parent.

  A run paused inside a subgraph or a graph tool before this release starts that subgraph or
  graph again when resumed, and a paused or failed parent runs its finished children once more.
- **Fan-in arrivals and failures** (`adk-graph`): an entry of `{node}_fan_in` is the value the
  predecessor wrote to a channel named after itself, or else every update it wrote, never the
  whole state. A predecessor that ran but routed elsewhere no longer arrives.
  `GraphError::FanInIncomplete` reports a join whose remaining predecessors cannot run and
  that received fewer than its `min_predecessors`.
