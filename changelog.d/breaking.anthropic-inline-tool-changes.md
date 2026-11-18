- **`adk-anthropic` gains tool-change variants** (`adk-anthropic`): `ContentBlock` gains
  `ToolAddition` and `ToolRemoval`, and `MessageRole` gains `System`; an exhaustive `match`
  adds the arms. `AnthropicConfig` gains `inline_tool_changes` (`adk-model`).
