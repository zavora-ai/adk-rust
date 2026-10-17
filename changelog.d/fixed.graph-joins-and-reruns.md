- **Graph joins wait only for predecessors that can still run** (`adk-graph`): a node with
  several incoming edges counts an arrival only from a predecessor that routed to it, and is
  released once no missing predecessor can still run in the current pass. A merge after a
  conditional branch, and a node re-entered by a loop's back edge — the reviewers of
  `examples/coding_graph` — run instead of being skipped while the run returned `Ok`. When
  nothing else can run, every waiting join is released, so a run never ends with one that did
  not fire. A join's arrival is the predecessor's own updates, so `{node}_fan_in` no longer
  doubles on every pass of a loop. A `GraphAgentBuilder` graph defers a node with several
  incoming edges, as a `StateGraph` does.
- **An error from the agent in an `AgentNode` fails the node** (`adk-graph`): a model
  failure, guardrail block, or budget stop no longer becomes an empty success, so the node's
  retry policy applies and the nodes after it do not run.
- **Re-entered graph work runs again** (`adk-graph`): a checkpointed `SubgraphNode` runs on
  every pass of a loop and every turn, a node in a loop gets its own child's output from
  `run_node` on every pass, and a second call of a checkpointed `NodeTool` graph runs the graph
  instead of returning the first call's state.
- **A retried parent is served its finished children** (`adk-graph`): `run_node` numbers
  children per attempt, so a parent retried after a later child fails does not repeat a
  child that completed, such as a charge. A child with its own `with_node_retry` policy is
  retried under it.
- **Timeout policies keep a node's context** (`adk-graph`): a node under a `TimeoutPolicy`
  keeps its parent schema and child invoker, so a `SubgraphNode` or a node calling `run_node`
  no longer fails with an "executor defect" error.
