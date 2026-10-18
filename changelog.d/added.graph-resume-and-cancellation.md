- **`NodeContext::resume_value`** (`adk-graph`): the answer to a node's own dynamic pause,
  supplied with `ExecutionConfig::with_resume_value(node_name, value)` and present only in the
  step that resumes it, so a later pause asks again. `GraphAgent` answers with the turn's
  text, or with what `GraphAgentBuilder::resume_mapper` returns. A `SubgraphNode` passes its
  answer to the node inside that paused.
- **`ExecutionConfig::with_cancellation_token`** (`adk-graph`, `functional`): cancels an
  `#[entrypoint]` run; the run returns `GraphError::Cancelled` and starts no further task.
- **`InterruptedExecution::interrupts`** and
  **`GraphToolConfirmationPause::all_from_interrupted_execution`** (`adk-graph`): every pause
  of the step that stopped the run.
- **`examples/graph_runtime`**: agent nodes reading session state through templates, node
  events streamed and recorded through `GraphAgent` and `Runner`, a failed step that stops
  dispatch, a join reached from `START`, retry delays within `max_delay`, two pauses in one
  step, scoped resume answers, agent error categories, single-flight children, and a retried
  pause and a cancelled `#[entrypoint]` workflow, over the in-memory, SQLite and Redis
  checkpointers against a scripted OpenAI-compatible server, live Gemini, and the live OpenAI
  endpoint.
