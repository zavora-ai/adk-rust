- **Managed custom tool results and confirmations reach the agent** (`adk-managed`): a
  `user.custom_tool_result` is no longer held behind the five-minute park and then
  discarded — the model continues from the client's result instead of a
  `pending_client_execution` placeholder. `user.tool_confirmation` now decides the call the
  agent held, which then executes or reports the denial. `DefaultModelResolver` constructs
  models under the new provider features instead of failing for every provider. Fixes #753.
