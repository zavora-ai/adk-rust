- **Workflow agents pause on a held tool call and resume where they stopped**
  (`adk-agent`): `SequentialAgent`, `LoopAgent`, `ParallelAgent`, `ConditionalAgent` and
  `LlmConditionalAgent` stop when a sub-agent holds a call for confirmation, instead of running
  their later sub-agents in the same run, and skip their after-agent callbacks until the run
  that finishes them. The next run resumes at the holding sub-agent instead of restarting the
  workflow: earlier sub-agents, finished parallel branches, completed loop iterations, the
  condition and the routing classification are not repeated.
- **Calls without IDs get generated `adk-call-` IDs** (`adk-agent`, `adk-core`): a function
  call a provider sends without an ID (Ollama, tool-call markup) gets an ID prefixed with
  `adk_core::GENERATED_CALL_ID_PREFIX` when the model turn is normalized, instead of the
  `{invocation}_{name}_{index}` ID generated at dispatch. The saved model event, its
  responses, its confirmation request and `ToolContext::function_call_id()` carry it.
  `adk_agent::normalize_content` assigns them (`Content::assign_call_ids`), and `LlmAgent`
  removes generated IDs from model requests (`Content::strip_generated_call_ids`), so providers
  receive what they did before.
- **Compaction never covers a held call** (`adk-runner`): session compaction, intra-invocation
  compaction and `apply_compaction_with_retry` stop before the earliest call still held for
  confirmation (`adk_core::held_call_compaction_limit`). A `CompactionStrategy` receives only the
  events before it; the held call and every later event are kept unchanged, so a session with an
  undecided call keeps those events until the call is answered.
- **`EventActions` has a `workflow_positions` field** (`adk-core`): a map of
  `WorkflowPosition`s recording where each workflow agent stopped on a confirmation event. A
  struct literal that names every field needs it or `..Default::default()`; it is omitted from
  serialized events when empty. `Session` gains `workflow_resume_positions`, defaulting to none;
  event-log sessions return `adk_core::workflow_resume_positions`.
