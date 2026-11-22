- **`static_instruction` sends an instruction exactly as written** (`adk-agent`):
  `LlmAgentBuilder::static_instruction` and `CodeActAgentBuilder::static_instruction` take
  text whose braces are not placeholders, such as a skill file that quotes
  `set_target {window_id}`, and send it before the global instruction without reading
  `{key}` placeholders. Being the same in every request, it stays in a provider's prompt
  cache. `instruction` and the providers are unchanged.
