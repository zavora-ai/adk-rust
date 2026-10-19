- **Two static tools with one name fail the agent build** (`adk-agent`, `adk-realtime`,
  `adk-core`): `LlmAgentBuilder::build`, `CodeActAgentBuilder::build`, and
  `RealtimeAgentBuilder::build` return an `agent.duplicate_tool_name` error
  (`adk_core::DUPLICATE_TOOL_NAME_CODE`), and `RealtimeRunnerBuilder::build` and
  `IntegratedRealtimeRunnerBuilder::build` return a config error, when two registered tools
  share a name, instead of letting the last registration win. `adk_core::ensure_unique_tool_names`
  runs the same check for other builders.
