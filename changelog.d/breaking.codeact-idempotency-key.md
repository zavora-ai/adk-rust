- **`CodeActCheckpoint` has a `run_id` field** (`adk-agent`, `codeact` feature):
  `run_id: Option<String>` records the agent run that wrote the checkpoint, so a call replayed
  from it in a later invocation keeps its `ToolContext::idempotency_key()`. A struct literal
  must set it. Checkpoints written by 2.3.0 deserialize with `None` and resume under a new run
  ID.
