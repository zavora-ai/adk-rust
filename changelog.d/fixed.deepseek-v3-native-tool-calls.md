- **DeepSeek-V3 native tool-call tokens become calls** (`adk-model`): a DeepSeek-V3 model
  served without a tool-call parser writes `<｜tool▁call▁begin｜>function<｜tool▁sep｜>NAME`
  and a fenced arguments object into its text (or the V3.1 `NAME<｜tool▁sep｜>{...}` form). The
  OpenAI-compatible and Ollama clients now turn these into function calls for declared tools,
  streamed or not; the streaming buffer also recognises the opening `<｜tool` token, which it
  missed before. `OllamaModel` now recognises every text-encoded tool-call format in streamed
  responses, not only in complete ones.
