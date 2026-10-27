- **Requested calls** (`adk-core`, `adk-agent`, `adk-server`, `adk-harness`): a
  `RequestedCall` is a tool call a person asked for through something an agent drew. A run
  carries it as `RunConfig::requested_call`; the root `LlmAgent` takes it before its first
  model call, runs the before-tool stages a model's call passes, and authorizes it as
  `GovernedCall::requested`, so it is held for the person as the agent itself, under its own
  policy, standing rules and approval store, or refused, and the run ends without a model
  call. No earlier approval, standing rule, Go ahead mode or confirmation handler answers it,
  a looser mode does not cancel it, and it resumes only on a person's decision. The model
  reads it through a runtime notice (`RequestedCall::notice`) with the arguments and the
  answer inside `<untrusted-content>` blocks, never as a call it made and never as the
  person's words, so no provider receives a function call it did not issue. Durable runs
  take it from the input `{"text": ..., "requestedCall": ...}` of an authenticated run, and
  `Harness::send_requested` sends one without a kernel. `CodeActAgent`, the workflow agents,
  teams and `GraphAgent` refuse requested calls; a custom agent refuses with
  `RequestedCall::refusal_events`. See `docs/official_docs/security/tool-authorization.md`.
