- **Answers to graph pauses** (`adk-graph`): `ExecutionConfig::resume_values` and
  `with_resume_value(key, value)` supply the answer to a pause, keyed by the name of the graph
  node that paused or, for an `#[entrypoint]` workflow, by the interrupt's continuation key, such
  as `interrupt-1`. `NodeContext::resume_value` hands a node that paused with a dynamic interrupt
  its answer in the step that resumes it only, so a later pause asks again. `GraphAgent` answers
  with the turn's text, or with what `GraphAgentBuilder::resume_mapper` returns. A `SubgraphNode`
  passes its answer to the node inside that paused.
- **`ExecutionConfig::with_cancellation_token`** (`adk-graph`, `functional`): cancels an
  `#[entrypoint]` run; the run returns `GraphError::Cancelled`, starts no further task, and
  writes no further checkpoint.
- **Every tool-confirmation pause of a step** (`adk-graph`):
  `GraphToolConfirmationPause::all_from_interrupted_execution` reads each one from
  `InterruptedExecution::interrupts`, and `GraphToolConfirmationPause::from_interrupt` reads one.
- **`graph_runtime` example** (`examples/graph_runtime`): agent nodes reading session state
  through templates, node events streamed and recorded through `GraphAgent` and `Runner`, a
  failed step that stops dispatch, a join reached from `START`, retry delays within `max_delay`,
  two pauses in one step, scoped resume answers, agent error categories, single-flight children,
  and a retried pause and a cancelled `#[entrypoint]` workflow, over the in-memory, SQLite, and
  Redis checkpointers against a scripted OpenAI-compatible server, live Gemini, and the live
  OpenAI endpoint.
