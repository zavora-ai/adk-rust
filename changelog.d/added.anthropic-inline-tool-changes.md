- **Tools added mid-conversation keep the prompt cache** (`adk-model`, `adk-anthropic`):
  `AnthropicConfig::with_inline_tool_changes(true)` keeps each conversation's `tools` array as
  first sent and sends a tool added, redefined or removed later as a `tool_addition` or
  `tool_removal` block in a mid-conversation system message (beta `inline-tools-2026-09-15`).
  Measured on Claude Haiku 5.5, the request after a tool addition read 5,787 cached tokens instead
  of 0. A conversation whose history changed sends its tools as they are; a provider-native
  compaction carries the changes so the summary records them. `adk-anthropic` gains
  `ToolAdditionBlock`, `ToolRemovalBlock`, `ToolChangeTarget`, `MessageRole::System` and
  `MessageCreateParams::requires_inline_tools_beta`.
