- **Agent nodes see the caller's session** (`adk-graph`): under a `Runner`, an agent inside an
  `AgentNode` sees the session's real id and invocation id, and its session state is the
  caller's, read live, with the graph's state layered on top, so an instruction template such
  as `{customer_name}` resolves instead of failing the node.
- **`GraphAgent` streams and records its agents' events** (`adk-graph`): each agent node's
  events reach the caller as the agent produces them, on the node's branch and under the
  turn's invocation id, so a `Runner` streams them and the session keeps the inner turns.
  The graph's own output and pause events carry the turn's invocation id and the graph
  agent's name.
- **A failed step stops dispatching** (`adk-graph`): after the first failure no error handler
  recovers, a node still queued for the step does not start, and a sibling waiting to retry
  stops waiting, so the run reports the failure without running the queued nodes' side
  effects or waiting for the longest retry chain. Finished nodes keep their writes for the
  resume.
- **A node reached from `START` and from another node runs once** (`adk-graph`): it waits
  for its other predecessors like any join, instead of running at the start and again after
  them.
- **Retry delays stay within `max_delay`** (`adk-graph`): the delay is capped at
  `max_delay / (1 + jitter)` before jitter is applied, so the default policy's full jitter no
  longer sleeps up to twice the cap.
- **Every pause of a step reaches the caller** (`adk-graph`): `GraphAgent` yields one event
  per pause, and a stream yields one `Interrupted` event per pause, so two agent nodes asking
  to confirm a tool call in the same step are both answered in one turn. In `Messages` mode a
  node's pause no longer stops the rest of its step.
- **An agent node's failure keeps its category** (`adk-graph`): the turn's error is the
  agent's own `AdkError`, so a rate-limited model is still `RateLimited`, retryable, and HTTP
  429, and a node timeout stays a timeout.
- **Concurrent children under one run id run once** (`adk-graph`): a second `run_node_with`
  call under an identity that is running waits for it and is served its output.
- **`#[task(retry)]` does not retry a pause** (`adk-rust-macros`, `adk-graph`): a suspension at
  `TaskContext::interrupt`, or a cancellation, ends the task at once under the interrupt's own
  key and is not recorded as a failed task.
