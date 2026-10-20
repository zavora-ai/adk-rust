- **`adk-rust` keeps long-term facts by default** (`adk-cli`): the REPL, `code` and `goal`
  make one extra model call when a session ends, to distil it into facts stored in
  `harness.db`, and send fact text to the model provider's embedder (or Gemini or OpenAI
  when that key is set) for recall. Pass `--no-memory`, or `--ephemeral`, to keep no facts.
  `serve` keeps none.
