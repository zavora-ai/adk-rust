- **Agent node failures carry the agent's error** (`adk-graph`): an `AgentNode`'s failure is
  `GraphError::AgentFailed { node, source }`, where `source` is the agent's `AdkError`, instead
  of `GraphError::NodeExecutionFailed`; a failure inside a subgraph names the path, such as
  `drafting.writer`. `From<GraphError> for AdkError` returns that error with the node named in
  its message, so a rate-limited model stays `RateLimited`, retryable, and HTTP 429.
- **Graph errors are converted, not flattened** (`adk-graph`): `GraphAgent` yields
  `AdkError::from(error)` instead of `AdkError::agent(message)`, and `NodeTool` returns it
  instead of `AdkError::tool(message)`, so a node timeout stays a timeout. A node error that
  already names its node — `NodeExecutionFailed`, `NodeTimedOut` — ends the run as it is instead
  of wrapped in another `NodeExecutionFailed`.
- **Functional suspension and cancellation** (`adk-graph`): `From<FunctionalError> for
  GraphError` maps a suspension to `GraphError::Suspended { continuation_key, message }` and a
  cancellation to `GraphError::Cancelled` instead of `GraphError::Other`.
- **Every pause of a step** (`adk-graph`): `InterruptedExecution` gains `interrupts`, every pause
  of the step in node-name order with `interrupt` the first, so a struct literal must set it
  (`InterruptedExecution::new` does). `executor::SuperStepResult::interrupt: Option<Interrupt>`
  is replaced by `interrupts: Vec<(String, Interrupt)>`, and the struct gains `updates`, each
  executed node's own updates.
- **`GraphAgent` yields its agent nodes' events** (`adk-graph`): each agent node's events reach
  the caller as the agent produces them, on the node's branch and under the turn's invocation
  id, before the output mapper's events, so a `Runner` streams them and the session records the
  inner turns. Every event the graph agent builds carries the turn's invocation id instead of
  `"graph_output"` or `"graph_interrupted"`, and the graph agent's name as author when the
  mapper sets none; read a pause with `GraphInterruptPayload::from_event`.
- **Agent nodes run under the caller's identity** (`adk-graph`): with a parent invocation, an
  `AgentNode`'s agent reports the caller's session id and invocation id instead of the graph's
  thread id and a fresh invocation id, and its session reports the caller's app name and user id
  instead of `graph_app` and `graph_user`.
- **`ExecutionConfig` cannot be built with a struct literal** (`adk-graph`): it gains the public
  fields `resume_values`, `durability`, and, with `functional`, `cancellation_token`, and a
  crate-private field, so code outside `adk-graph` builds it with `ExecutionConfig::new` and its
  builder methods, and assigns public fields afterwards.
