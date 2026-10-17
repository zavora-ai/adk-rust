- **Realtime tool results can be screened before the model sees them** (`adk-realtime`):
  `RealtimeAgent` and `RealtimeRunner` had no result-aware hook, so a tool's result went to the
  provider, and `RealtimeAgent`'s tool event to the session, as the tool returned it.
  `tool_result_screen` on `RealtimeAgentBuilder`, `RealtimeRunnerBuilder`, and
  `IntegratedRealtimeRunnerBuilder` takes an `adk_core::ToolResultScreen` that rewrites or
  withholds each result after the after-tool stages; a screen that cannot decide withholds the
  result.
