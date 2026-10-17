- **Workflow agents pause on a held tool call and resume where they stopped**
  (`adk-agent`): `SequentialAgent`, `LoopAgent`, `ParallelAgent`, `ConditionalAgent` and
  `LlmConditionalAgent` stop when a sub-agent holds a call for confirmation and skip their
  after-agent callbacks until the run that finishes them. The next run resumes at the holding
  sub-agent instead of restarting the workflow: earlier sub-agents, finished parallel branches,
  completed loop iterations, the condition and the routing classification are not repeated.
  Code that relied on later sub-agents running in the run that held a call, or on a resume
  restarting the workflow, changes.
- **Calls without IDs get generated `adk-call-` IDs** (`adk-agent`, `adk-core`): a function call
  a provider sends without an ID (Ollama, tool-call markup) carries a generated ID
  (`GENERATED_CALL_ID_PREFIX`) in the saved model event, its responses, its confirmation request,
  `ToolContext::function_call_id()` and its idempotency key, in place of the
  `{invocation}_{turn}_{name}_{index}` ID generated at dispatch, which remains only for calls
  a plugin adds after the model event is saved. `adk_agent::normalize_content` assigns them.
  `LlmAgent` removes generated IDs from model requests, so providers receive what they did before.
- **Compaction never covers a held call** (`adk-runner`): session compaction, intra-invocation
  compaction and `apply_compaction_with_retry` stop before the earliest call still held for
  confirmation (`adk_core::held_call_compaction_limit`). A `CompactionStrategy` receives only the
  events before it; the held call and every later event are kept unchanged, so a session with an
  undecided call keeps those events until the call is answered.
- **`EventActions::workflow_positions`** (`adk-core`): new field (with `WorkflowPosition`)
  recording where each workflow agent stopped on a confirmation event. `Session` gains
  `workflow_resume_positions`, defaulting to none; event-log sessions return
  `adk_core::workflow_resume_positions`.
