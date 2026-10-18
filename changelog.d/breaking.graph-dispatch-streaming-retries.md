- **Graph errors and pauses carry more structure** (`adk-graph`):
  - An agent node's failure is `GraphError::AgentFailed { node, source }`, where `source` is
    the agent's `AdkError`, instead of `GraphError::NodeExecutionFailed`. A failure inside a
    subgraph names the path, such as `drafting.writer`. `From<GraphError> for AdkError`
    returns the agent's error, and `GraphAgent` yields it instead of
    `AdkError::agent(message)`.
  - A node error that already names the node — `NodeExecutionFailed`, `NodeTimedOut` — ends
    the run as it is instead of wrapped in another `NodeExecutionFailed`.
  - `NodeTool` reports a graph failure with `AdkError::from` instead of `AdkError::tool`.
  - New variants `GraphError::Suspended` and `GraphError::Cancelled`;
    `From<FunctionalError> for GraphError` maps the functional suspension and cancellation to
    them instead of `GraphError::Other`.
  - `InterruptedExecution` has an `interrupts` field, and
    `executor::SuperStepResult::interrupt: Option<Interrupt>` is now
    `interrupts: Vec<(String, Interrupt)>`.
- **`GraphAgent` events** (`adk-graph`): a turn yields the agent nodes' events before the
  output mapper's, and every event it builds carries the turn's invocation id instead of
  `"graph_output"` or `"graph_interrupted"`; read a pause with
  `GraphInterruptPayload::from_event`. A step with several pauses yields one event per pause.
- **Agent node context** (`adk-graph`): with a parent invocation, the agent's `session_id()`
  is the caller's session id instead of the graph's thread id, its `invocation_id()` is the
  caller's, and its session state reads through to the caller's session.
- **`ExecutionConfig` gains a crate-private field**, so it cannot be built with a struct
  literal outside `adk-graph`; use `ExecutionConfig::new` and its builder methods.
- **`ExecutionConfig::resume_values`** (`adk-graph`): a graph node that paused with a dynamic
  interrupt now reads the value supplied under its name.
- **An entry node that is also a join waits** (`adk-graph`): a node reached from `START` and
  from another node runs once its other predecessors have, instead of at the start.
