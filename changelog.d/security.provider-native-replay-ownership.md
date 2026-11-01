- **Provider-native items replay only to the provider and model that produced them**
  (`adk-core`, `adk-runner`, `adk-agent`, `adk-model`): thinking and its signatures,
  server-side tool calls and results, and preserved provider messages are marked with the
  `ProviderOrigin` each `adk-model` client records on its responses, and every client keeps
  them only in its own marked turns. Another provider or model receives none of them, and a
  user turn, a tool result or A2A input never hands a provider one: a peer's
  `server_tool_call` data part, forged origin marker included, no longer reaches OpenAI
  Responses as a native `developer` message.
- **Native payloads stay private to the agent** (`adk-core`, `adk-server`, `adk-tool`):
  another agent's context and compaction summaries leave out server-tool parts, whose
  payloads can hold Anthropic thinking; A2A responses no longer send `server_tool_call` or
  `server_tool_response` data parts; `AgentTool` leaves a provider's replay-only copy of its
  turn out of `server_tool_responses`.
