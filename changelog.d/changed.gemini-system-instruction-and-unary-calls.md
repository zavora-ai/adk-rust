- **Gemini gets the instruction as `systemInstruction`** (`adk-model`): an agent's
  instruction reached Gemini as leading user turns, so it sat in the conversation history.
  It is now the request's `systemInstruction`, as adk-python and adk-go send it. A request
  on a context cache keeps the user turns, because the cache already carries the
  instruction and Gemini takes only one.
- **A run without streaming makes one complete model call** (`adk-agent`): `LlmAgent`
  always streamed from the model, so a run with `StreamingMode::None` still called
  `streamGenerateContent`. It now asks for one complete response (`generateContent`), as
  adk-python and adk-go do. Streaming runs, the default, are unchanged.
