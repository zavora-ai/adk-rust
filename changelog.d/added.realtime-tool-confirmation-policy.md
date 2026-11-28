- **Tool confirmation rules on the realtime builders** (`adk-realtime`):
  `require_tool_confirmation(name)`, `require_tool_confirmation_for_all()` and
  `tool_confirmation_policy(policy)` on `RealtimeAgentBuilder`, `RealtimeRunnerBuilder` and
  `IntegratedRealtimeRunnerBuilder` mark calls that need a person's approval, as
  `LlmAgentBuilder` does.
