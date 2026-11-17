- **Gemini 3.8 Live and Gemini 3.8 Live Extended Thinking** (`adk-realtime`, `adk-model`,
  `adk-gemini`): both models are catalogued and priced, and the Live setup follows each one's
  rules. On `gemini-3.8-live`, whose own default is non-blocking, a tool is declared
  `BLOCKING` unless its `ToolDefinition` sets `ToolBehavior::NonBlocking`, so the turn that
  calls it answers with its result, and no thinking level is sent (the model closes the
  socket with 1007 on one). On `gemini-3.8-live-extended-thinking`, tools are declared
  `NON_BLOCKING`, and `RealtimeConfig::with_thinking_level(Effort)` sets its reasoning level. `gemini-3.1-flash-live-preview` stays the default and its setup is
  unchanged. The catalog dates the Vertex AI `gemini-live-2.5-flash-native-audio`
  retirement (2026-12-13) and names `gemini-3.8-live` as the replacement Google gives.
