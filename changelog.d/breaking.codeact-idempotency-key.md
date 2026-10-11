- **`CodeActAgent` idempotency keys name the run and the model turn** (`adk-agent`):
  `ToolContext::idempotency_key()` for a CodeAct tool call is
  `{app}/{user}/{session}/codeact/{run_id}/{iteration}/{call_id}` instead of
  `{app}/{user}/{session}/{invocation}/{call_id}`. Interpreter call ids restart with every
  program, so the old key was shared by calls of different model turns, and a recovered
  call in a later invocation got a new key. Tools that pass the key to a downstream API
  receive the new format.
  - `CodeActCheckpoint` has a new `run_id: Option<String>` field; a struct literal must
    set it. Checkpoints written before the field existed deserialize with `None` and
    resume under a new run ID.
