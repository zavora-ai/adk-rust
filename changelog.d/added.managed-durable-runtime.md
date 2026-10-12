- **Managed sessions run on the durable runtime** (`adk-managed`): every turn runs under a
  `DurableRunStore` run on a per-session queue; a turn waiting on a custom tool result or a
  confirmation parks its run until `DurableConfig::pending_expiry` (five minutes by
  default), enforced by the store. The client's answer is recorded in an `ApprovalStore`
  and wakes the run, through any runtime instance over the same `ManagedStores`; an
  approval service set with `ManagedStores::with_approval_service` decides confirmations
  instead. A runtime instance rebuilds a stored session the first time a method names it,
  and `DefaultManagedAgentRuntime::recover` hosts every stored session with a run in
  progress. New: `ManagedStores`, `DurableConfig`, `with_stores`, `with_state_store`,
  `with_durable_config`, `durability`.
- **`SessionServiceStateStore`** (`adk-managed`): keeps managed session state in any
  `SessionService` backend, writing only the events added since the previous save.
- **Model provider features** (`adk-managed`, `adk-rust`): `gemini`, `openai`,
  `anthropic`, `ollama`, and `all-providers` let `DefaultModelResolver` build those
  providers, with `DefaultModelResolver::with_api_key` in place of the environment. The
  umbrella crate's provider features enable them alongside `managed-runtime`.
