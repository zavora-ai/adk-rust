- **`AnthropicConfig::with_thinking_display`** (`adk-model`): sets `thinking.display` for the
  configured thinking mode. Current Claude models omit the thinking text by default, so
  `ThinkingDisplay::Summarized` is needed for `Part::Thinking` to carry readable text.
  `ThinkingDisplay` is re-exported from `adk_model::anthropic`.
