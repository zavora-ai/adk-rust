- **Agent instructions travel as `system` contents** (`adk-agent`, `adk-model`): `LlmAgent`
  sends its global instruction, instruction and output-schema directive with role
  `"system"` instead of `"user"`. The Anthropic provider places them in the `system`
  parameter on every request and no longer moves leading user messages into it, so a
  session's first user message stays a user message and the cached prefix stays
  stable from the first request. Standard Gemini `generateContent` requests are
  unchanged. Custom `Llm` implementations receive instructions as `system` contents.
