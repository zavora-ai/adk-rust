- **Managed custom tool results and confirmations reach the agent** (`adk-managed`): a
  `user.custom_tool_result` is no longer held behind the 300 s park and then discarded —
  the model continues from the client's result instead of a `pending_client_execution`
  placeholder. `user.tool_confirmation` now decides the call the agent held, which then
  executes or reports the denial. `DefaultModelResolver` constructs models instead of
  failing for every provider. `DefaultManagedAgentRuntime` writes to the state store it is
  given. Fixes #753.
