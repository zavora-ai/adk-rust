- **Requested calls** (`adk-core`, `adk-agent`, `adk-server`, `adk-harness`): a
  `RequestedCall` is a tool call a person asked for through something an agent drew. A run
  carries it as `RunConfig::requested_call`; the root `LlmAgent` takes it before its first
  model call and authorizes it as `GovernedCall::requested`, so it is held for the person as
  the agent itself, under its own policy, standing rules and approval store, or refused, and
  the run ends without a model call. No earlier approval, standing rule, Go ahead mode or
  confirmation handler answers it, and it resumes only on a person's decision. The model
  reads it as the person's request and its answer as the person's line, never as a call it
  made, so no provider receives a function call it did not issue. Durable runs
  take it from the input `{"text": ..., "requestedCall": ...}` of an authenticated run, and
  `Harness::send_requested` sends one without a kernel. `CodeActAgent` refuses requested
  calls. See `docs/official_docs/security/tool-authorization.md`.
