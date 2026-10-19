- **Agent nodes read the caller's session state** (`adk-graph`): under a `Runner`, an
  `AgentNode`'s agent reads the caller's session state live, with the graph's state layered on
  top, so an instruction template such as `{customer_name}` resolves instead of failing the node.
  The agent's own state writes stay local to the node.
- **A failed step stops dispatching** (`adk-graph`): after the first failure no error handler
  recovers, a node still queued for the step does not start, and a sibling waiting to retry
  stops waiting, so the run reports the failure without running the queued nodes' side effects
  or waiting for the longest retry chain.
- **A node reached from `START` and from another node waits for it** (`adk-graph`): it runs once
  its other predecessors have, like any join, instead of at the start without their output.
- **Retry delays stay within `max_delay`** (`adk-graph`): the delay is capped at
  `max_delay / (1 + jitter)` before jitter is applied, so the default policy's full jitter no
  longer sleeps up to twice the cap.
- **Every pause of a step reaches the caller** (`adk-graph`): `GraphAgent` yields one event per
  pause, and a stream yields one `Interrupted` event per pause, so two agent nodes asking to
  confirm a tool call in the same step are both answered in one turn. In `Messages` mode a
  node's pause no longer stops the rest of its step.
- **Concurrent children under one run id run once** (`adk-graph`): a second `run_node_with` call
  under an identity that is running waits for it and is served its output.
- **`#[task(retry)]` does not retry a pause** (`adk-rust-macros`, `adk-graph`): a suspension at
  `TaskContext::interrupt`, or a cancellation, ends the task at once, keeps the interrupt's
  continuation key, and is not recorded as a failed task.
