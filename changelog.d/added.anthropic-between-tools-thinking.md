- **Thinking off on Claude Sonnet 5.5** (`adk-anthropic`, `adk-model`): `ThinkingConfig::between_tools()`
  and `ThinkingMode::BetweenTools` send `thinking: {"type": "between_tools"}`, the setting Claude
  Sonnet 5.5 accepts in place of `{"type": "disabled"}`. The model does not think before answering,
  and the updates it writes between tool calls come back as thinking blocks.
